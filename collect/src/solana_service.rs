use crate::common::retry_blocking;
use crate::common::QuadraticBackoffStrategy;
use crate::marinade_service::fetch_bonds;
use crate::releases::firedancer_lineage;
use crate::slot_params::SLOTS_IN_EPOCH;
use crate::validator_version::ValidatorVersion;
use crate::validators::*;
use anyhow::Context;
use bincode::deserialize;
use csv::{required, Column};
use log::{info, warn};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use solana_account::Account;
use solana_account_decoder::validator_info;
use solana_account_decoder::{UiAccountData, UiAccountEncoding, UiDataSliceConfig};
use solana_clock::{Epoch, Slot};
use solana_commitment_config::CommitmentConfig;
use solana_config_interface::state::{get_config_data, ConfigKeys};
use solana_pubkey::Pubkey;
use solana_rpc_client::http_sender::HttpSender;
use solana_rpc_client::rpc_client::{RpcClient, RpcClientConfig};
use solana_rpc_client_api::client_error::{Error as ClientError, Result as ClientResult};
use solana_rpc_client_api::config::{
    RpcAccountInfoConfig, RpcEpochConfig, RpcProgramAccountsConfig,
};
use solana_rpc_client_api::custom_error::JSON_RPC_SCAN_ERROR;
use solana_rpc_client_api::error_object::RpcErrorObject;
use solana_rpc_client_api::filter::{Memcmp, RpcFilterType};
use solana_rpc_client_api::request::{RpcError, RpcRequest, MAX_MULTIPLE_ACCOUNTS};
use solana_rpc_client_api::response::{RpcVoteAccountInfo, RpcVoteAccountStatus};
use solana_sdk_ids::sysvar::stake_history;
use solana_stake_interface as stake;
use solana_stake_interface::stake_history::{StakeHistory, StakeHistoryEntry};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    io::{BufReader, Read},
    str::FromStr,
    sync::OnceLock,
    thread::sleep,
    time::{Duration, Instant},
};

const RPC_STAKE_ACCOUNTS_FETCH_BACKOFF_MS: u64 = 200;
// The blocking client polls its sockets only during a call, so it cannot notice a server closing an idle one.
const RPC_POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
const WITHDRAW_AUTHORITY_OFFSET: usize = 4 + 8 + 32;
const STAKE_VARIANT_TAG: [u8; 4] = 2u32.to_le_bytes();
const VOTER_OFFSET: usize = WITHDRAW_AUTHORITY_OFFSET + 32 + 8 + 8 + 32;
const STAKE_OFFSET: usize = VOTER_OFFSET + 32;
const ACTIVATION_EPOCH_OFFSET: usize = STAKE_OFFSET + 8;
const DEACTIVATION_EPOCH_OFFSET: usize = ACTIVATION_EPOCH_OFFSET + 8;
const STAKE_FIELDS: UiDataSliceConfig = UiDataSliceConfig {
    offset: WITHDRAW_AUTHORITY_OFFSET,
    length: DEACTIVATION_EPOCH_OFFSET + 8 - WITHDRAW_AUTHORITY_OFFSET,
};
const MAX_STAKE_ACCOUNTS_STREAM_BYTES: u64 = 1 << 30;
const MAX_STAKE_ACCOUNTS_PER_FETCH: u64 = MAX_STAKE_ACCOUNTS_STREAM_BYTES / 1024;
const MAX_WITHDRAWER_PREFIX_LEN: usize = 4;
const MAX_VOTER_PREFIX_LEN: usize = 1;
const MAX_GET_INFLATION_REWARD_ADDRESSES: usize = 32;

pub fn solana_client(url: String, commitment: String) -> RpcClient {
    RpcClient::new_with_commitment(url, CommitmentConfig::from_str(&commitment).unwrap())
}

pub fn solana_client_with_timeout(url: String, timeout: Duration, commitment: String) -> RpcClient {
    let http = reqwest::Client::builder()
        .default_headers(HttpSender::default_headers())
        .timeout(timeout)
        .pool_idle_timeout(RPC_POOL_IDLE_TIMEOUT)
        .build()
        .expect("RPC HTTP client must build");
    RpcClient::new_sender(
        HttpSender::new_with_client(url, http),
        RpcClientConfig::with_commitment(CommitmentConfig::from_str(&commitment).unwrap()),
    )
}

pub fn get_program_accounts_with_config(
    rpc_client: &RpcClient,
    program_id: &Pubkey,
    config: RpcProgramAccountsConfig,
) -> ClientResult<Vec<(Pubkey, Account)>> {
    rpc_client
        .get_program_ui_accounts_with_config(program_id, config)?
        .into_iter()
        .map(|(pubkey, ui_account)| {
            ui_account
                .to_account()
                .map(|account| (pubkey, account))
                .ok_or_else(|| {
                    ClientError::new_with_request(
                        RpcError::ParseError("Account from rpc".to_string()).into(),
                        RpcRequest::GetProgramAccounts,
                    )
                })
        })
        .collect()
}

pub fn get_stake_history(rpc_client: &RpcClient) -> anyhow::Result<StakeHistory> {
    Ok(bincode::deserialize(
        &rpc_client.get_account_data(&stake_history::ID)?,
    )?)
}

pub type EpochCreditsEntry = (Epoch, u64, u64);

// SIMD-0033 timely vote credits: at most 16 credits for each slot.
pub const MAX_TOWER_CREDITS_IN_EPOCH: u64 = 16 * SLOTS_IN_EPOCH;

// The agave Alpenglow vote reward code writes this marker in the migration epoch.
// Entries before it count tower vote credits, entries after it count reward lamports.
const EPOCH_CREDITS_MIGRATION_MARKER: Epoch = Epoch::MAX;

#[derive(Clone, Copy)]
enum CreditsRegime {
    Tower,
    Migration,
    Alpenglow,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpochCredits {
    pub tower_credits: Option<u64>,
    pub vote_reward_lamports: Option<u64>,
}

fn epoch_credits_delta(entries: &[EpochCreditsEntry], epoch: Epoch) -> Option<Option<u64>> {
    entries
        .iter()
        .find(|(record_epoch, _, _)| *record_epoch == epoch)
        .map(|(_, end_credits, start_credits)| end_credits.checked_sub(*start_credits))
}

pub fn split_epoch_credits(
    vote_account: &RpcVoteAccountInfo,
    epoch: Epoch,
    alpenglow_migration_epoch: Option<Epoch>,
) -> Option<EpochCredits> {
    let history = vote_account.epoch_credits.as_slice();
    let marker = history
        .iter()
        .position(|(record_epoch, _, _)| *record_epoch == EPOCH_CREDITS_MIGRATION_MARKER);
    let account_alpenglow_migration_epoch = marker
        .and_then(|m| history.get(m + 1))
        .map(|(record_epoch, _, _)| *record_epoch)
        .or(alpenglow_migration_epoch);
    let regime = match account_alpenglow_migration_epoch {
        Some(m) if epoch == m => CreditsRegime::Migration,
        Some(m) if epoch > m => CreditsRegime::Alpenglow,
        _ => CreditsRegime::Tower,
    };

    // Split the epoch delta at the marker: the entry before it goes to `tower`, the entry after it to `alpenglow`.
    // With no marker, give the one delta to the column of the regime.
    let (tower, alpenglow) = match marker {
        Some(m) => (
            epoch_credits_delta(&history[..m], epoch),
            epoch_credits_delta(&history[m + 1..], epoch),
        ),
        None => {
            let delta = epoch_credits_delta(history, epoch);
            match regime {
                CreditsRegime::Tower => (delta, None),
                // Testnet 1042: 4 of 469 accounts voted Tower, earned no Alpenglow reward, and got no marker.
                CreditsRegime::Migration
                    if delta
                        .flatten()
                        .is_none_or(|delta| delta <= MAX_TOWER_CREDITS_IN_EPOCH) =>
                {
                    (delta, None)
                }
                CreditsRegime::Migration | CreditsRegime::Alpenglow => (None, delta),
            }
        }
    };
    // Agave adds an entry only for a reward above 0, so a staked account with older entries earned 0.
    // `activated_stake` is the stake in the current epoch. `close_epoch` checks the stake of `epoch` again.
    let alpenglow = match (regime, alpenglow) {
        (CreditsRegime::Alpenglow, None)
            if vote_account.activated_stake > 0
                && history
                    .iter()
                    .any(|(record_epoch, _, _)| *record_epoch < epoch) =>
        {
            Some(Some(0))
        }
        (_, alpenglow) => alpenglow,
    };
    let epoch_in_window = |entries: &[EpochCreditsEntry]| {
        entries
            .first()
            .map_or(marker.is_none(), |(first_epoch, _, _)| {
                *first_epoch <= epoch
            })
    };
    let tower_entries = marker.map_or(history, |m| &history[..m]);
    // No entry for the epoch: 0 tower credits before Alpenglow, None after it.
    // Also None when the epoch is older than the entries `epochCredits` keeps.
    let tower = match (regime, tower) {
        (CreditsRegime::Alpenglow, _) => None,
        (_, None) if epoch_in_window(tower_entries) => Some(Some(0)),
        (_, tower) => tower,
    };
    if tower.is_none() && alpenglow.is_none() {
        return None;
    }

    Some(EpochCredits {
        tower_credits: tower.flatten(),
        vote_reward_lamports: alpenglow.flatten(),
    })
}

pub fn latest_credits_total(history: &[EpochCreditsEntry]) -> Option<u64> {
    history
        .iter()
        .rev()
        .find(|(record_epoch, _, _)| *record_epoch != EPOCH_CREDITS_MIGRATION_MARKER)
        .map(|(_, end_credits, _)| *end_credits)
}

#[derive(Deserialize)]
struct AgGenesisCert {
    block: AgGenesisCertBlock,
}

#[derive(Deserialize)]
struct AgGenesisCertBlock {
    slot: Slot,
}

const JSON_RPC_METHOD_NOT_FOUND: i64 = -32601;

// `getAgGenesisCert` is null until the first Alpenglow block. On testnet that block came 4999 slots
// after the feature gate activated, both in epoch 1042.
pub fn is_alpenglow_active(client: &RpcClient) -> anyhow::Result<Option<Epoch>> {
    let cert: Option<AgGenesisCert> = match client.send(
        RpcRequest::Custom {
            method: "getAgGenesisCert",
        },
        Value::Null,
    ) {
        Ok(cert) => cert,
        // The node runs Agave older than 4.3.
        Err(err)
            if matches!(
                err.kind(),
                solana_rpc_client_api::client_error::ErrorKind::RpcError(
                    solana_rpc_client_api::request::RpcError::RpcResponseError {
                        code: JSON_RPC_METHOD_NOT_FOUND,
                        ..
                    }
                )
            ) =>
        {
            warn!(
                "RPC has no getAgGenesisCert, so the cluster counts as not migrated to Alpenglow"
            );
            None
        }
        Err(err) => return Err(err.into()),
    };
    let Some(cert) = cert else {
        return Ok(None);
    };
    let genesis_epoch = client.get_epoch_schedule()?.get_epoch(cert.block.slot);
    info!(
        "Alpenglow genesis block at slot {}, epoch {genesis_epoch}",
        cert.block.slot
    );
    Ok(Some(genesis_epoch))
}

const CLIENT_IDS_CSV: &str = include_str!("../client-ids.csv");

struct ClientRegistry {
    names: HashMap<u16, String>,
    ids_by_name: HashMap<String, u16>,
}

const CLIENT_ID_COLUMNS: [Column; 2] = [required("client_id"), required("client_name")];

#[derive(Deserialize)]
struct ClientIdRow {
    client_id: u16,
    client_name: String,
}

fn parse_client_registry(text: &str) -> anyhow::Result<ClientRegistry> {
    let mut names = HashMap::new();
    let mut ids_by_name = HashMap::new();

    for row in csv::parse::<ClientIdRow>(text, &CLIENT_ID_COLUMNS, "client-ids.csv")? {
        ids_by_name.insert(canonical_client_name(&row.client_name), row.client_id);
        names.insert(row.client_id, row.client_name);
    }

    Ok(ClientRegistry { names, ids_by_name })
}

fn client_registry() -> &'static ClientRegistry {
    static REGISTRY: OnceLock<ClientRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        parse_client_registry(CLIENT_IDS_CSV).unwrap_or_else(|err| panic!("{err:#}"))
    })
}

// Agave renders registry names without the separators the CSV uses ("AgaveBam" vs "Agave Bam").
fn canonical_client_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// How one registry id groups. `label()` renders the lineage and the engine for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClientGrouping {
    /// Who ships the binary.
    vendor: &'static str,
    /// Which codebase it forks.
    lineage: &'static str,
    /// The block engine the binary runs. `None` for a client running on its own.
    engine: Option<&'static str>,
}

impl ClientGrouping {
    const fn new(
        vendor: &'static str,
        lineage: &'static str,
        engine: Option<&'static str>,
    ) -> Self {
        Self {
            vendor,
            lineage,
            engine,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientId {
    Registered(u16),
    Unrecognized(Option<u16>),
    Missing,
}

// The gossip client id is a number; the responding RPC node renders it to a name from its own
// compiled-in table and falls back to "Unknown(N)", so both forms have to resolve to the same id.
pub fn resolve_client_id(client_id: Option<&str>) -> ClientId {
    let Some(raw) = client_id.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return ClientId::Missing;
    };
    let registry = client_registry();

    if let Some(number) = raw
        .strip_prefix("Unknown(")
        .and_then(|rest| rest.strip_suffix(')'))
        .and_then(|number| number.trim().parse::<u16>().ok())
    {
        return if registry.names.contains_key(&number) {
            ClientId::Registered(number)
        } else {
            ClientId::Unrecognized(Some(number))
        };
    }

    match registry.ids_by_name.get(&canonical_client_name(raw)) {
        Some(id) => ClientId::Registered(*id),
        None => ClientId::Unrecognized(None),
    }
}

impl ClientId {
    // Registry-only: an id we cannot classify must not vary with the answering RPC's rendering.
    pub fn number(&self) -> Option<u16> {
        let ClientId::Registered(id) = self else {
            return None;
        };
        Some(*id)
    }

    pub fn name(&self) -> Option<&'static str> {
        let ClientId::Registered(id) = self else {
            return None;
        };
        client_registry().names.get(id).map(String::as_str)
    }

    pub fn vendor(&self) -> Option<&'static str> {
        self.groupings().map(|grouping| grouping.vendor)
    }

    pub fn lineage(&self, version: Option<&str>) -> Option<&'static str> {
        let lineage = self.groupings()?.lineage;
        let version = version.and_then(|version| ValidatorVersion::from_gossip(version).ok());
        Some(match (self, version) {
            // Frankendancer + JitoBAM also reported id 12 until FrankenBAM got id 14.
            (ClientId::Registered(12), Some(version)) => firedancer_lineage(&version),
            _ => lineage,
        })
    }

    pub fn engine(&self) -> Option<&'static str> {
        self.groupings().and_then(|grouping| grouping.engine)
    }

    /// The lineage as display text, and the block engine after it: `Agave + Jito`.
    pub fn label(&self, version: Option<&str>) -> Option<String> {
        let grouping = self.groupings()?;
        let mut label = self.lineage(version)?.to_string();
        label[..1].make_ascii_uppercase();
        if let Some(engine) = grouping.engine {
            label.push_str(" + ");
            label.push_str(engine);
        }

        Some(label)
    }

    // The registry assigns an id per lineage variant of a vendor; id 12 is the shared-id exception.
    fn groupings(&self) -> Option<ClientGrouping> {
        let ClientId::Registered(id) = self else {
            return None;
        };
        // Ids 2 and 5 run no engine: the bundle tile is a config flag in the same binary, so gossip
        // cannot tell a bundle-running node from a plain one, and neither claim is observable.
        Some(match id {
            // Id 0 is Agave's pre-rename vendor, not a fork of it, so it runs no engine like id 3.
            0 => ClientGrouping::new("solana-labs", "agave", None),
            1 => ClientGrouping::new("jito", "agave", Some("Jito")),
            2 => ClientGrouping::new("frankendancer", "frankendancer", None),
            3 => ClientGrouping::new("agave", "agave", None),
            4 => ClientGrouping::new("paladin", "agave", Some("Paladin")),
            5 => ClientGrouping::new("firedancer", "firedancer", None),
            6 => ClientGrouping::new("bam", "agave", Some("JitoBAM")),
            7 => ClientGrouping::new("sig", "sig", None),
            8 => ClientGrouping::new("rakurai", "agave", Some("Rakurai")),
            9 => ClientGrouping::new("harmonic", "firedancer", Some("Harmonic")),
            10 => ClientGrouping::new("harmonic", "agave", Some("Harmonic")),
            11 => ClientGrouping::new("harmonic", "frankendancer", Some("Harmonic")),
            12 => ClientGrouping::new("bam", "firedancer", Some("JitoBAM")),
            13 => ClientGrouping::new("raiku", "agave", Some("Raiku")),
            14 => ClientGrouping::new("bam", "frankendancer", Some("JitoBAM")),
            _ => return None,
        })
    }
}

// A malformed gossip version is dropped so store never replaces the last known good version with it.
pub fn is_plausible_node_version(version: &str) -> bool {
    crate::validator_version::ValidatorVersion::from_gossip(version).is_ok()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeContact {
    pub ip: Option<String>,
    pub gossip_port: Option<u16>,
    pub version: Option<String>,
    pub client_id: Option<u16>,
    pub client_id_raw: Option<String>,
    pub feature_set: Option<u32>,
    pub shred_version: Option<u16>,
    pub rpc_public: bool,
    pub pubsub_public: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RpcContactInfoExt {
    pubkey: String,
    gossip: Option<String>,
    rpc: Option<String>,
    pubsub: Option<String>,
    version: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
    feature_set: Option<u32>,
    shred_version: Option<u16>,
}

pub fn get_cluster_nodes_info(
    rpc_client: &RpcClient,
) -> anyhow::Result<HashMap<String, NodeContact>> {
    info!("Getting cluster nodes info");
    let raw: Vec<RpcContactInfoExt> = rpc_client.send(RpcRequest::GetClusterNodes, Value::Null)?;

    let mut out: HashMap<String, NodeContact> = HashMap::with_capacity(raw.len());
    // Counted and reported once per run: a client the registry predates appears on every node
    // running it, and a per-node warning at that volume is what got the previous one ignored.
    let mut unclassified_renderings: HashMap<String, usize> = HashMap::new();
    for node in raw {
        let version = node.version.and_then(|v| {
            let version = v
                .split_once(char::is_whitespace)
                .map(|(version, extra)| {
                    warn!(
                        "Node {} has version: {version} with extra info: {extra}",
                        node.pubkey
                    );
                    version.to_string()
                })
                .unwrap_or(v);
            if !is_plausible_node_version(&version) {
                warn!(
                    "Node {} reports malformed version: '{version}', ignoring",
                    node.pubkey
                );
                return None;
            }
            Some(version)
        });

        let (ip, gossip_port) = node
            .gossip
            .as_deref()
            .and_then(parse_socket_addr)
            .map(|(ip, port)| (Some(ip), Some(port)))
            .unwrap_or((None, None));

        let resolved = resolve_client_id(node.client_id.as_deref());
        if matches!(resolved, ClientId::Unrecognized(_)) {
            let rendering = node.client_id.as_deref().unwrap_or_default().trim();
            *unclassified_renderings
                .entry(rendering.to_string())
                .or_default() += 1;
        }

        out.insert(
            node.pubkey.clone(),
            NodeContact {
                ip,
                gossip_port,
                version,
                client_id: resolved.number(),
                client_id_raw: node.client_id,
                feature_set: node.feature_set,
                shred_version: node.shred_version,
                rpc_public: node.rpc.is_some(),
                pubsub_public: node.pubsub.is_some(),
            },
        );
    }

    if !unclassified_renderings.is_empty() {
        warn!(
            "Client ids missing from client-ids.csv, so these nodes stay unclassified: {}",
            unclassified_clients_summary(&unclassified_renderings)
        );
    }

    Ok(out)
}

// Ordered by node count, so whichever client is worth adding to client-ids.csv first comes first.
fn unclassified_clients_summary(renderings: &HashMap<String, usize>) -> String {
    let mut renderings: Vec<_> = renderings.iter().collect();
    renderings.sort_by(|(left_name, left_count), (right_name, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_name.cmp(right_name))
    });
    renderings
        .into_iter()
        .map(|(rendering, count)| format!("{rendering} on {count} node(s)"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse_socket_addr(s: &str) -> Option<(String, u16)> {
    let (ip, port) = s.rsplit_once(':')?;
    let port = port.parse().ok()?;
    let ip = ip.trim_start_matches('[').trim_end_matches(']').to_string();
    Some((ip, port))
}

pub fn get_total_activated_stake(vote_accounts: &RpcVoteAccountStatus) -> (u64, u64) {
    (
        vote_accounts
            .current
            .iter()
            .map(|v| v.activated_stake)
            .sum(),
        vote_accounts
            .delinquent
            .iter()
            .map(|v| v.activated_stake)
            .sum(),
    )
}

pub fn get_minimum_superminority_stake(vote_accounts: &RpcVoteAccountStatus) -> u64 {
    let mut activated_stakes: Vec<_> = vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
        .map(|v| v.activated_stake)
        .collect();
    let total_activated_stake: u64 = activated_stakes.iter().sum();
    let superminority_threshold = total_activated_stake / 3;
    activated_stakes.sort_by(|a, b| b.cmp(a));

    let mut accumulated = 0;
    let mut last_stake = 0;
    for stake in activated_stakes.iter() {
        accumulated += stake;
        last_stake = *stake;
        if accumulated > superminority_threshold {
            break;
        }
    }

    last_stake
}

fn parse_validator_info(
    pubkey: &Pubkey,
    account: &Account,
) -> anyhow::Result<(Pubkey, ValidatorInfo)> {
    if account.owner != solana_config_interface::id() {
        anyhow::bail!("{pubkey} is not a validator info account");
    }
    let key_list: ConfigKeys = deserialize(&account.data)?;
    if key_list.keys.len() > 1 && key_list.keys.contains(&(validator_info::id(), false)) {
        let (validator_pubkey, _) = key_list.keys[1];
        let validator_info_string: String = deserialize(get_config_data(&account.data)?)?;
        let validator_info: Map<_, _> = serde_json::from_str(&validator_info_string)?;
        Ok((
            validator_pubkey,
            ValidatorInfo {
                name: extract_json_value(&validator_info, "name".to_string()),
                url: extract_json_value(&validator_info, "website".to_string()),
                details: extract_json_value(&validator_info, "details".to_string()),
                keybase: extract_json_value(&validator_info, "keybaseUsername".to_string()),
                icon_url: extract_json_value(&validator_info, "iconUrl".to_string()),
            },
        ))
    } else {
        anyhow::bail!("{pubkey} could not be parsed as a validator info account");
    }
}
fn get_program_accounts_base64_zstd(
    rpc_client: &RpcClient,
    program_id: &Pubkey,
) -> ClientResult<Vec<(Pubkey, Account)>> {
    get_program_accounts_with_config(
        rpc_client,
        program_id,
        RpcProgramAccountsConfig {
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64Zstd),
                ..RpcAccountInfoConfig::default()
            },
            ..RpcProgramAccountsConfig::default()
        },
    )
}

pub fn get_validators_info(
    rpc_client: &RpcClient,
    rpc_attempts: usize,
) -> anyhow::Result<HashMap<String, ValidatorInfo>> {
    info!("Getting validator info");
    let validator_info = retry_blocking(
        || get_program_accounts_base64_zstd(rpc_client, &solana_config_interface::id()),
        QuadraticBackoffStrategy::iter_durations(rpc_attempts),
        |err, attempt, backoff| {
            warn!(
                "Attempt {} to get validator info has failed: {}, retrying in {:?} seconds",
                attempt,
                err,
                backoff.as_secs()
            )
        },
    )?;

    let mut validator_info_map = HashMap::new();
    if validator_info.is_empty() {
        warn!("No validator info accounts found");
    }
    for (validator_info_pubkey, validator_info_account) in validator_info.iter() {
        match parse_validator_info(validator_info_pubkey, validator_info_account) {
            Ok((validator_pubkey, validator_info)) => {
                validator_info_map.insert(validator_pubkey.to_string(), validator_info);
            }
            Err(err) => warn!("Couldn't parse validator info {err}"),
        }
    }

    Ok(validator_info_map)
}

fn extract_json_value(json: &Map<String, Value>, key: String) -> Option<String> {
    json.get(&key)
        .and_then(|value| serde_json::from_value(value.clone()).ok())
}

// Bincode discriminants of VoteStateVersions; 0 is Uninitialized and carries no withdrawer to read.
const VOTE_STATE_VERSION_UNINITIALIZED: u32 = 0;
const VOTE_STATE_VERSION_V1_14_11: u32 = 1;
const VOTE_STATE_VERSION_V3: u32 = 2;
const VOTE_STATE_VERSION_V4: u32 = 3;

const VOTE_AUTHORIZED_WITHDRAWER_OFFSET: usize = 4 + 32;
// Byte 68 is the v1/v3 commission and the v4 collector, so reads past it dispatch on version.
const VOTE_PRE_V4_COMMISSION_OFFSET: usize = 68;
const VOTE_V4_INFLATION_REWARDS_COLLECTOR_OFFSET: usize = 68;
const VOTE_V4_BLOCK_REVENUE_COLLECTOR_OFFSET: usize = 100;
const VOTE_V4_INFLATION_REWARDS_COMMISSION_BPS_OFFSET: usize = 132;
const VOTE_V4_BLOCK_REVENUE_COMMISSION_BPS_OFFSET: usize = 134;
const VOTE_V4_PENDING_DELEGATOR_REWARDS_OFFSET: usize = 136;
// Bincode `Option` tag of 1 byte, then the key.
const VOTE_V4_BLS_PUBKEY_COMPRESSED_OFFSET: usize = 144;
pub const BLS_PUBKEY_COMPRESSED_SIZE: usize = 48;

// Names and null semantics match solana-snapshot-parser's ValidatorMeta so both stay reconcilable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoteStateFields {
    pub authorized_withdrawer: Pubkey,
    // None where this build does not know the commission offset; the withdrawer still reads.
    pub inflation_rewards_commission_bps: Option<u16>,
    pub inflation_rewards_commission_bps_is_v4: Option<bool>,
    pub inflation_rewards_collector: Option<Pubkey>,
    pub block_revenue_collector: Option<Pubkey>,
    pub block_revenue_commission_bps: Option<u16>,
    pub pending_delegator_rewards: Option<u64>,
    pub bls_pubkey: Option<[u8; BLS_PUBKEY_COMPRESSED_SIZE]>,
    // Set by the scan, not by the parser, which sees only the account data.
    pub lamports: Option<u64>,
}

fn read_array<const N: usize>(data: &[u8], offset: usize, field: &str) -> anyhow::Result<[u8; N]> {
    data.get(offset..offset + N)
        .and_then(|slice| slice.try_into().ok())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "vote account holds {} bytes, too few to read {field} at {offset}",
                data.len()
            )
        })
}

fn read_pubkey(data: &[u8], offset: usize, field: &str) -> anyhow::Result<Pubkey> {
    Ok(Pubkey::new_from_array(read_array::<32>(
        data, offset, field,
    )?))
}

fn read_bls_pubkey(data: &[u8]) -> anyhow::Result<Option<[u8; BLS_PUBKEY_COMPRESSED_SIZE]>> {
    match read_array::<1>(
        data,
        VOTE_V4_BLS_PUBKEY_COMPRESSED_OFFSET,
        "bls_pubkey_compressed",
    )?[0]
    {
        0 => Ok(None),
        1 => Ok(Some(read_array::<BLS_PUBKEY_COMPRESSED_SIZE>(
            data,
            VOTE_V4_BLS_PUBKEY_COMPRESSED_OFFSET + 1,
            "bls_pubkey_compressed",
        )?)),
        tag => anyhow::bail!("bls_pubkey_compressed holds the Option tag {tag}"),
    }
}

pub fn parse_vote_state(data: &[u8]) -> anyhow::Result<VoteStateFields> {
    let version = u32::from_le_bytes(read_array::<4>(data, 0, "the version discriminant")?);
    let authorized_withdrawer = read_pubkey(
        data,
        VOTE_AUTHORIZED_WITHDRAWER_OFFSET,
        "authorized_withdrawer",
    )?;

    match version {
        VOTE_STATE_VERSION_UNINITIALIZED => {
            anyhow::bail!("vote state version {version} carries no authorized withdrawer")
        }
        VOTE_STATE_VERSION_V1_14_11 | VOTE_STATE_VERSION_V3 => {
            let commission = read_array::<1>(data, VOTE_PRE_V4_COMMISSION_OFFSET, "commission")?[0];
            Ok(VoteStateFields {
                authorized_withdrawer,
                // agave synthesizes the same projection, so the runtime applies it either way.
                inflation_rewards_commission_bps: Some(u16::from(commission).saturating_mul(100)),
                inflation_rewards_commission_bps_is_v4: Some(false),
                inflation_rewards_collector: None,
                block_revenue_collector: None,
                block_revenue_commission_bps: None,
                pending_delegator_rewards: None,
                bls_pubkey: None,
                lamports: None,
            })
        }
        VOTE_STATE_VERSION_V4 => Ok(VoteStateFields {
            authorized_withdrawer,
            inflation_rewards_commission_bps: Some(u16::from_le_bytes(read_array::<2>(
                data,
                VOTE_V4_INFLATION_REWARDS_COMMISSION_BPS_OFFSET,
                "inflation_rewards_commission_bps",
            )?)),
            inflation_rewards_commission_bps_is_v4: Some(true),
            inflation_rewards_collector: Some(read_pubkey(
                data,
                VOTE_V4_INFLATION_REWARDS_COLLECTOR_OFFSET,
                "inflation_rewards_collector",
            )?),
            block_revenue_collector: Some(read_pubkey(
                data,
                VOTE_V4_BLOCK_REVENUE_COLLECTOR_OFFSET,
                "block_revenue_collector",
            )?),
            block_revenue_commission_bps: Some(u16::from_le_bytes(read_array::<2>(
                data,
                VOTE_V4_BLOCK_REVENUE_COMMISSION_BPS_OFFSET,
                "block_revenue_commission_bps",
            )?)),
            pending_delegator_rewards: Some(u64::from_le_bytes(read_array::<8>(
                data,
                VOTE_V4_PENDING_DELEGATOR_REWARDS_OFFSET,
                "pending_delegator_rewards",
            )?)),
            bls_pubkey: read_bls_pubkey(data)?,
            lamports: None,
        }),
        // An unknown version still yields the withdrawer; its SIMD-0185 offsets may have moved.
        _ => Ok(VoteStateFields {
            authorized_withdrawer,
            inflation_rewards_commission_bps: None,
            inflation_rewards_commission_bps_is_v4: None,
            inflation_rewards_collector: None,
            block_revenue_collector: None,
            block_revenue_commission_bps: None,
            pending_delegator_rewards: None,
            bls_pubkey: None,
            lamports: None,
        }),
    }
}

// Half the fleet unparsed is a layout change, not the usual few uninitialized accounts.
const MAX_UNPARSED_VOTE_ACCOUNTS_PERCENT: usize = 50;

// Relies on the vote account layout and needs updating if any field position changes.
pub fn get_vote_account_states(
    rpc_client: &RpcClient,
) -> anyhow::Result<HashMap<String, VoteStateFields>> {
    info!("Getting vote account states");
    let vote_program_id = solana_sdk_ids::vote::id();
    let vote_accounts = get_program_accounts_base64_zstd(rpc_client, &vote_program_id)?;
    let accounts: Vec<(String, &[u8])> = vote_accounts
        .iter()
        .map(|(account_pubkey, account)| (account_pubkey.to_string(), account.data.as_slice()))
        .collect();

    let mut states = parse_vote_account_states(&accounts)?;
    for (account_pubkey, account) in vote_accounts.iter() {
        if let Some(state) = states.get_mut(&account_pubkey.to_string()) {
            state.lamports = Some(account.lamports);
        }
    }
    Ok(states)
}

fn parse_vote_account_states(
    accounts: &[(String, &[u8])],
) -> anyhow::Result<HashMap<String, VoteStateFields>> {
    let mut states: HashMap<String, VoteStateFields> = HashMap::with_capacity(accounts.len());
    let mut unparsed = 0usize;
    let mut first_error = None;
    for (account_pubkey, data) in accounts.iter() {
        match parse_vote_state(data) {
            Ok(state) => {
                states.insert(account_pubkey.clone(), state);
            }
            Err(err) => {
                unparsed += 1;
                // Aggregated: one line each would be thousands on a cluster-wide shape change
                first_error.get_or_insert_with(|| format!("{account_pubkey}: {err}"));
            }
        }
    }
    if let Some(first_error) = first_error {
        // A run writing zero self stake fleet-wide leaves nothing saying the data was wrong.
        if unparsed * 100 > accounts.len() * MAX_UNPARSED_VOTE_ACCOUNTS_PERCENT {
            anyhow::bail!(
                "Could not parse {unparsed} of {} vote accounts, first: {first_error}",
                accounts.len()
            );
        }
        warn!(
            "Could not parse {unparsed} of {} vote accounts, first: {first_error}",
            accounts.len()
        );
    }
    let v4 = states
        .values()
        .filter(|state| state.inflation_rewards_commission_bps_is_v4 == Some(true))
        .count();
    // Self stake survives these; silence is how a version bump would reach close_epoch unnoticed.
    let without_commission = states
        .values()
        .filter(|state| state.inflation_rewards_commission_bps.is_none())
        .count();
    if without_commission > 0 {
        warn!(
            "{without_commission} vote accounts hold a version this build reads no commission from"
        );
    }
    // Logged at 0 too: that is the baseline MAX_UNPARSED_VOTE_ACCOUNTS_PERCENT gets tuned on.
    info!(
        "Parsed {} vote accounts, {v4} on vote state v4, unparsed {unparsed}",
        states.len()
    );

    Ok(states)
}

pub fn withdraw_authorities(
    vote_account_states: &HashMap<String, VoteStateFields>,
) -> HashSet<(String, String)> {
    vote_account_states
        .iter()
        .map(|(vote_account, state)| {
            (
                state.authorized_withdrawer.to_string(),
                vote_account.clone(),
            )
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectorHealth {
    pub owner: Option<Pubkey>,
    pub lamports: u64,
    // None where the account is missing: agave pays it only when the payout alone covers rent.
    pub healthy: Option<bool>,
}

// agave credits the vote account without the SIMD-0232 collector checks, so there is nothing to check.
pub fn checked_collector(vote_account: &str, collector: Option<Pubkey>) -> Option<Pubkey> {
    collector.filter(|collector| collector.to_string() != vote_account)
}

pub fn collector_health_of<'a>(
    vote_account: &str,
    state: Option<&VoteStateFields>,
    health: &'a HashMap<Pubkey, CollectorHealth>,
) -> (Option<&'a CollectorHealth>, Option<&'a CollectorHealth>) {
    let health_of = |collector: Option<Pubkey>| {
        checked_collector(vote_account, collector).and_then(|c| health.get(&c))
    };
    (
        health_of(state.and_then(|state| state.inflation_rewards_collector)),
        health_of(state.and_then(|state| state.block_revenue_collector)),
    )
}

// relax_post_exec_min_balance_check is active, so agave checks rent only on an account it creates.
fn evaluate_collector(collector: &Pubkey, account: Option<&Account>) -> CollectorHealth {
    let healthy = if *collector == solana_sdk_ids::incinerator::id() {
        Some(false)
    } else {
        account.map(|account| solana_sdk_ids::system_program::check_id(&account.owner))
    };
    CollectorHealth {
        owner: account.map(|account| account.owner),
        lamports: account.map_or(0, |account| account.lamports),
        healthy,
    }
}

pub fn get_collector_health(
    rpc_client: &RpcClient,
    vote_account_states: &HashMap<String, VoteStateFields>,
) -> anyhow::Result<HashMap<Pubkey, CollectorHealth>> {
    let checked = |kind: fn(&VoteStateFields) -> Option<Pubkey>| -> HashSet<Pubkey> {
        vote_account_states
            .iter()
            .filter_map(|(vote_account, state)| checked_collector(vote_account, kind(state)))
            .collect()
    };
    let inflation = checked(|state| state.inflation_rewards_collector);
    let block_revenue = checked(|state| state.block_revenue_collector);
    let collectors: Vec<Pubkey> = inflation.union(&block_revenue).copied().collect();
    info!("Checking {} distinct collector accounts", collectors.len());

    let mut health = HashMap::with_capacity(collectors.len());
    for chunk in collectors.chunks(MAX_MULTIPLE_ACCOUNTS) {
        for (collector, account) in chunk.iter().zip(rpc_client.get_multiple_accounts(chunk)?) {
            health.insert(*collector, evaluate_collector(collector, account.as_ref()));
        }
    }

    for (kind, collectors) in [
        ("inflation rewards", &inflation),
        ("block revenue", &block_revenue),
    ] {
        let mut unhealthy = collectors.iter().filter(|collector| {
            health
                .get(collector)
                .is_some_and(|h| h.healthy == Some(false))
        });
        if let Some(first) = unhealthy.next() {
            warn!(
                "{} {kind} collectors would burn their commission, first: {first}",
                unhealthy.count() + 1
            );
        }
    }

    Ok(health)
}

// solana-client 2.2 RpcInflationReward predates commission_bps and would drop it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RpcInflationRewardExt {
    #[allow(dead_code)]
    epoch: Epoch,
    #[allow(dead_code)]
    amount: u64,
    commission: Option<u8>,
    commission_bps: Option<u16>,
}

#[derive(Debug, Default)]
struct CommissionStats {
    from_commission: usize,
    from_bps: usize,
    lossy: usize,
    disagree: usize,
    unresolved: usize,
    no_reward: usize,
}

// Rounds up where agave's commission_percent() floors: down reads just over the 10% cap as at it.
pub fn bps_to_percent(bps: u16) -> u8 {
    bps.min(10_000).div_ceil(100) as u8
}

fn resolve_commission_percent(
    commission: Option<u8>,
    commission_bps: Option<u16>,
    stats: &mut CommissionStats,
) -> Option<u8> {
    // Basis points win: they are the finer-grained source, and `commission` is only ever a
    // projection of them once SIMD-0291 is active.
    if let Some(bps) = commission_bps {
        if commission.is_some_and(|commission| bps_to_percent(bps) != commission) {
            stats.disagree += 1;
        }
        if bps % 100 != 0 {
            stats.lossy += 1;
        }
        stats.from_bps += 1;
        return Some(bps_to_percent(bps));
    }

    if let Some(commission) = commission {
        stats.from_commission += 1;
        return Some(commission);
    }

    stats.unresolved += 1;
    None
}

pub fn get_commission_from_inflation_rewards(
    rpc_client: &RpcClient,
    vote_accounts: &RpcVoteAccountStatus,
    epoch: Option<Epoch>,
) -> anyhow::Result<HashMap<String, u8>> {
    let vote_addresses: Vec<_> = vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
        .map(|v| Pubkey::from_str(&v.vote_pubkey).unwrap())
        .collect();
    let mut result: HashMap<String, u8> = Default::default();
    let mut stats = CommissionStats::default();
    for vote_addresses_chunk in vote_addresses.chunks(MAX_GET_INFLATION_REWARD_ADDRESSES) {
        let addresses: Vec<String> = vote_addresses_chunk
            .iter()
            .map(|address| address.to_string())
            .collect();
        let rewards: Vec<Option<RpcInflationRewardExt>> = rpc_client.send(
            RpcRequest::GetInflationReward,
            json!([
                addresses,
                RpcEpochConfig {
                    epoch,
                    commitment: Some(rpc_client.commitment()),
                    min_context_slot: None,
                }
            ]),
        )?;
        result.extend(vote_addresses_chunk.iter().zip(rewards).filter_map(
            |(vote_address, reward)| {
                let Some(reward) = reward else {
                    stats.no_reward += 1;
                    return None;
                };
                let commission = resolve_commission_percent(
                    reward.commission,
                    reward.commission_bps,
                    &mut stats,
                )?;
                Some((vote_address.to_string(), commission))
            },
        ));
    }

    if stats.lossy > 0 || stats.disagree > 0 {
        warn!(
            "Commission from inflation rewards: {} rounded to a whole percent, {} disagreed between commission and commissionBps",
            stats.lossy, stats.disagree
        );
    }
    let queried = vote_addresses.len();
    info!(
        "Resolved commission for {} of {} validators: {} from commission, {} from commissionBps, {} without a reward, {} unresolved",
        result.len(),
        queried,
        stats.from_commission,
        stats.from_bps,
        stats.no_reward,
        stats.unresolved
    );
    if result.len() * 2 < queried {
        warn!("Resolved commission for fewer than half of the {queried} validators queried");
    }

    Ok(result)
}

#[derive(Debug, Default, Clone, Copy)]
pub struct StakeAccountTotals {
    // Only accounts whose withdrawer is the vote account's own, plus its bond.
    pub self_stake: u64,
    // Every stake account, whoever owns it.
    pub activating: u64,
    pub deactivating: u64,
}

#[allow(clippy::too_many_arguments)]
pub fn get_stake_account_totals(
    rpc_client: &RpcClient,
    epoch: Epoch,
    stake_history: &StakeHistory,
    bonds_url: &str,
    allow_zero_funded_bonds: bool,
    rpc_attempts: usize,
    rpc_timeout: Duration,
    allow_skipped_stake_pages: bool,
    vote_account_states: &HashMap<String, VoteStateFields>,
) -> anyhow::Result<HashMap<String, StakeAccountTotals>> {
    let mut totals = fetch_stake_account_totals(
        rpc_client,
        rpc_timeout,
        withdraw_authorities(vote_account_states),
        epoch,
        stake_history,
        rpc_attempts,
        allow_skipped_stake_pages,
    )?;

    // A pending-only entry fills the map without any self stake.
    assert!(
        totals.values().any(|t| t.self_stake != 0),
        "Failed to fetch self stake data"
    );

    let bonds = fetch_bonds(bonds_url)?;
    if bonds.is_empty() {
        anyhow::bail!(
            "Fetched empty bonds list from {bonds_url} for epoch {epoch}, expected at least one bond"
        );
    }
    if bonds.iter().all(|b| b.funded_amount == Decimal::ZERO) {
        if allow_zero_funded_bonds {
            warn!(
                "All {} bonds from {} for epoch {} have zero funded amounts",
                bonds.len(),
                bonds_url,
                epoch
            );
        } else {
            anyhow::bail!(
                "All {} bonds from {} for epoch {} have zero funded amounts, expected at least one non-zero amount",
                bonds.len(),
                bonds_url,
                epoch
            );
        }
    }

    for bond in bonds {
        let funded_amount_u64 = bond
            .funded_amount
            .to_u64()
            .ok_or_else(|| anyhow::anyhow!("Failed to convert Bond Decimal value to u64"))?;
        totals.entry(bond.vote_account).or_default().self_stake += funded_amount_u64;
    }
    Ok(totals)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StakeAccountsSlice {
    withdrawer_prefix: Vec<u8>,
    voter_prefix: Vec<u8>,
}

impl StakeAccountsSlice {
    fn page(page: u8) -> Self {
        Self {
            withdrawer_prefix: vec![page],
            voter_prefix: vec![],
        }
    }

    fn filters(&self) -> Vec<RpcFilterType> {
        let mut filters = vec![
            RpcFilterType::DataSize(200),
            RpcFilterType::Memcmp(Memcmp::new_raw_bytes(0, STAKE_VARIANT_TAG.to_vec())),
            RpcFilterType::Memcmp(Memcmp::new_raw_bytes(
                WITHDRAW_AUTHORITY_OFFSET,
                self.withdrawer_prefix.clone(),
            )),
        ];
        if !self.voter_prefix.is_empty() {
            filters.push(RpcFilterType::Memcmp(Memcmp::new_raw_bytes(
                VOTER_OFFSET,
                self.voter_prefix.clone(),
            )));
        }
        filters
    }

    fn split_range(&self) -> Option<UiDataSliceConfig> {
        let (offset, prefix_len, max_len) =
            if self.withdrawer_prefix.len() < MAX_WITHDRAWER_PREFIX_LEN {
                (
                    WITHDRAW_AUTHORITY_OFFSET,
                    self.withdrawer_prefix.len(),
                    MAX_WITHDRAWER_PREFIX_LEN,
                )
            } else {
                (VOTER_OFFSET, self.voter_prefix.len(), MAX_VOTER_PREFIX_LEN)
            };
        (prefix_len < max_len).then_some(UiDataSliceConfig {
            offset: offset + prefix_len,
            length: max_len - prefix_len,
        })
    }

    fn child(&self, byte: u8) -> Self {
        let mut child = self.clone();
        if child.withdrawer_prefix.len() < MAX_WITHDRAWER_PREFIX_LEN {
            child.withdrawer_prefix.push(byte);
        } else {
            child.voter_prefix.push(byte);
        }
        child
    }

    fn split(&self) -> Vec<Self> {
        (0..=u8::MAX).map(|byte| self.child(byte)).collect()
    }

    fn plan(&self, counts: &KeyCounts, max_accounts: u64) -> Vec<Self> {
        let keys: Vec<(&[u8], u64)> = counts.iter().map(|(k, n)| (k.as_slice(), *n)).collect();
        let mut planned = vec![];
        self.plan_into(&keys, max_accounts, &mut planned);
        planned
    }

    fn plan_into(&self, keys: &[(&[u8], u64)], max_accounts: u64, planned: &mut Vec<Self>) {
        let mut by_byte: BTreeMap<u8, Vec<(&[u8], u64)>> = BTreeMap::new();
        for (key, count) in keys {
            if let Some((byte, rest)) = key.split_first() {
                by_byte.entry(*byte).or_default().push((rest, *count));
            }
        }
        for (byte, rests) in by_byte {
            let child = self.child(byte);
            let total: u64 = rests.iter().map(|(_, count)| count).sum();
            if total <= max_accounts || rests.iter().all(|(rest, _)| rest.is_empty()) {
                planned.push(child);
            } else {
                child.plan_into(&rests, max_accounts, planned);
            }
        }
    }
}

impl std::fmt::Display for StakeAccountsSlice {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let joined = |prefix: &[u8]| {
            prefix
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join("/")
        };
        write!(f, "{}", joined(&self.withdrawer_prefix))?;
        if !self.voter_prefix.is_empty() {
            write!(f, " voter {}", joined(&self.voter_prefix))?;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SliceFetch<T> {
    Fetched(T),
    TooLarge,
}

type KeyCounts = HashMap<Vec<u8>, u64>;

fn walk_stake_account_slices<T>(
    max_accounts_per_fetch: u64,
    mut fetch: impl FnMut(&StakeAccountsSlice) -> anyhow::Result<SliceFetch<T>>,
    mut probe: impl FnMut(
        &StakeAccountsSlice,
        UiDataSliceConfig,
    ) -> anyhow::Result<SliceFetch<KeyCounts>>,
    mut visit: impl FnMut(&StakeAccountsSlice, T),
) -> anyhow::Result<Vec<StakeAccountsSlice>> {
    let mut pending: Vec<_> = (0..=u8::MAX).rev().map(StakeAccountsSlice::page).collect();
    let mut skipped = vec![];
    while let Some(slice) = pending.pop() {
        match fetch(&slice)
            .with_context(|| format!("Failed to fetch stake accounts on page {slice}"))?
        {
            SliceFetch::Fetched(accounts) => {
                visit(&slice, accounts);
                continue;
            }
            SliceFetch::TooLarge => info!(
                "Stake accounts on page {slice} exceed the {} MiB RPC response limit",
                MAX_STAKE_ACCOUNTS_STREAM_BYTES >> 20
            ),
        }
        let Some(range) = slice.split_range() else {
            skipped.push(slice);
            continue;
        };
        let children = match probe(&slice, range)
            .with_context(|| format!("Failed to probe stake accounts on page {slice}"))?
        {
            SliceFetch::Fetched(counts) => {
                let children = slice.plan(&counts, max_accounts_per_fetch);
                info!(
                    "Probed {} stake accounts on page {slice} into {} sub-pages",
                    counts.values().sum::<u64>(),
                    children.len()
                );
                children
            }
            SliceFetch::TooLarge => {
                warn!("Stake accounts on page {slice} exceed the RPC response limit even as a probe, splitting it into every next byte");
                slice.split()
            }
        };
        pending.extend(children.into_iter().rev());
    }
    Ok(skipped)
}

fn check_skipped_slices(
    skipped: &[StakeAccountsSlice],
    allow_skipped_stake_pages: bool,
) -> anyhow::Result<()> {
    if skipped.is_empty() {
        return Ok(());
    }
    let pages = skipped
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    if !allow_skipped_stake_pages {
        anyhow::bail!(
            "{} stake account pages exceed the RPC response limit at the finest split: {pages}; pass --allow-skipped-stake-pages to collect without them",
            skipped.len()
        );
    }
    warn!(
        "Stake account totals exclude {} oversized pages: {pages}",
        skipped.len()
    );
    Ok(())
}

#[derive(Default)]
struct SliceTotals {
    totals: HashMap<String, StakeAccountTotals>,
    self_stakes: u64,
}

impl SliceTotals {
    fn add(
        &mut self,
        stake_fields: &[u8],
        withdraw_authorities: &HashSet<(String, String)>,
        epoch: Epoch,
        stake_history: &StakeHistory,
    ) -> anyhow::Result<()> {
        let field = |offset: usize, length: usize| {
            let start = offset - STAKE_FIELDS.offset;
            stake_fields.get(start..start + length).with_context(|| {
                format!(
                    "Stake fields hold {} bytes, too few to read offset {offset}",
                    stake_fields.len()
                )
            })
        };
        let pubkey =
            |offset| -> anyhow::Result<Pubkey> { Ok(Pubkey::try_from(field(offset, 32)?)?) };
        let number = |offset| -> anyhow::Result<u64> {
            Ok(u64::from_le_bytes(field(offset, 8)?.try_into()?))
        };
        let withdrawer = pubkey(WITHDRAW_AUTHORITY_OFFSET)?;
        let voter = pubkey(VOTER_OFFSET)?;
        let mut delegation = stake::state::Delegation::new(
            &voter,
            number(STAKE_OFFSET)?,
            number(ACTIVATION_EPOCH_OFFSET)?,
        );
        delegation.deactivation_epoch = number(DEACTIVATION_EPOCH_OFFSET)?;
        #[allow(deprecated)]
        let StakeHistoryEntry {
            effective,
            activating,
            deactivating,
        } = delegation.stake_activating_and_deactivating(epoch, stake_history, None);
        let vote_key = voter.to_string();
        let is_self_stake = effective != 0
            && withdraw_authorities.contains(&(withdrawer.to_string(), vote_key.clone()));
        if !is_self_stake && activating == 0 && deactivating == 0 {
            return Ok(());
        }
        let totals = self.totals.entry(vote_key).or_default();
        totals.activating += activating;
        totals.deactivating += deactivating;
        if is_self_stake {
            self.self_stakes += 1;
            totals.self_stake += effective;
        }
        Ok(())
    }

    fn merge_into(self, totals: &mut HashMap<String, StakeAccountTotals>) {
        for (vote_key, slice_totals) in self.totals {
            let entry = totals.entry(vote_key).or_default();
            entry.self_stake += slice_totals.self_stake;
            entry.activating += slice_totals.activating;
            entry.deactivating += slice_totals.deactivating;
        }
    }
}

#[derive(Deserialize)]
struct KeyedAccountData {
    account: AccountData,
}

#[derive(Deserialize)]
struct AccountData {
    data: UiAccountData,
}

struct AccountsFold<'f, F>(&'f mut F);

impl<'de, F: FnMut(Vec<u8>) -> anyhow::Result<()>> serde::de::DeserializeSeed<'de>
    for AccountsFold<'_, F>
{
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de, F: FnMut(Vec<u8>) -> anyhow::Result<()>> serde::de::Visitor<'de> for AccountsFold<'_, F> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("an array of keyed accounts")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        use serde::de::Error;
        while let Some(KeyedAccountData { account }) = seq.next_element()? {
            let data = account
                .data
                .decode()
                .ok_or_else(|| A::Error::custom("undecodable account data"))?;
            (self.0)(data).map_err(|err| A::Error::custom(format!("{err:#}")))?;
        }
        Ok(())
    }
}

struct RpcResultFold<'f, F>(&'f mut F);

impl<'de, F: FnMut(Vec<u8>) -> anyhow::Result<()>> serde::de::DeserializeSeed<'de>
    for RpcResultFold<'_, F>
{
    type Value = Option<RpcErrorObject>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de, F: FnMut(Vec<u8>) -> anyhow::Result<()>> serde::de::Visitor<'de>
    for RpcResultFold<'_, F>
{
    type Value = Option<RpcErrorObject>;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON-RPC response")
    }

    fn visit_map<M: serde::de::MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
        let fold = self.0;
        let mut folded = false;
        let mut error = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "result" => {
                    map.next_value_seed(AccountsFold(&mut *fold))?;
                    folded = true;
                }
                "error" => error = Some(map.next_value()?),
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        if !folded && error.is_none() {
            return Err(serde::de::Error::custom(
                "RPC response holds neither a result nor an error",
            ));
        }
        Ok(error)
    }
}

fn fold_rpc_accounts(
    body: impl Read,
    cap: u64,
    mut fold: impl FnMut(Vec<u8>) -> anyhow::Result<()>,
) -> anyhow::Result<SliceFetch<()>> {
    use serde::de::DeserializeSeed;
    let mut reader = BufReader::new(body.take(cap + 1));
    let parsed = {
        let mut deserializer = serde_json::Deserializer::from_reader(&mut reader);
        RpcResultFold(&mut fold)
            .deserialize(&mut deserializer)
            .and_then(|error| deserializer.end().map(|()| error))
    };
    if reader.get_ref().limit() == 0 {
        return Ok(SliceFetch::TooLarge);
    }
    match parsed? {
        None => Ok(SliceFetch::Fetched(())),
        Some(error) if error.code == JSON_RPC_SCAN_ERROR => Ok(SliceFetch::TooLarge),
        Some(error) => anyhow::bail!("RPC error {}: {}", error.code, error.message),
    }
}

struct StakeAccountsRpc {
    http: reqwest::blocking::Client,
    rpc_url: String,
    commitment: CommitmentConfig,
    rpc_attempts: usize,
    rpc_timeout: Duration,
    requests: usize,
}

impl StakeAccountsRpc {
    fn scan<A>(
        &mut self,
        slice: &StakeAccountsSlice,
        data_slice: UiDataSliceConfig,
        init: impl Fn() -> A,
        fold: impl Fn(&mut A, Vec<u8>) -> anyhow::Result<()>,
    ) -> anyhow::Result<SliceFetch<A>> {
        let request = RpcRequest::GetProgramAccounts.build_request_json(
            1,
            json!([
                stake::program::ID.to_string(),
                RpcProgramAccountsConfig {
                    filters: Some(slice.filters()),
                    account_config: RpcAccountInfoConfig {
                        encoding: Some(UiAccountEncoding::Base64),
                        commitment: Some(self.commitment),
                        data_slice: Some(data_slice),
                        min_context_slot: None,
                    },
                    with_context: None,
                    sort_results: None,
                }
            ]),
        );
        self.requests += 1;
        let scanned = retry_blocking(
            || {
                let response = self
                    .http
                    .post(&self.rpc_url)
                    .timeout(self.rpc_timeout)
                    .json(&request)
                    .send()?
                    .error_for_status()?;
                let cap = MAX_STAKE_ACCOUNTS_STREAM_BYTES;
                if response.content_length().is_some_and(|length| length > cap) {
                    return Ok(SliceFetch::TooLarge);
                }
                let mut acc = init();
                Ok(
                    match fold_rpc_accounts(response, cap, |data| {
                        anyhow::ensure!(
                            data.len() == data_slice.length,
                            "RPC returned {} bytes for a {}-byte data slice",
                            data.len(),
                            data_slice.length
                        );
                        fold(&mut acc, data)
                    })? {
                        SliceFetch::Fetched(()) => SliceFetch::Fetched(acc),
                        SliceFetch::TooLarge => SliceFetch::TooLarge,
                    },
                )
            },
            QuadraticBackoffStrategy::iter_durations(self.rpc_attempts),
            |err, attempt, backoff| {
                warn!(
                    "Attempt {} has failed: {}, retrying in {:?} seconds",
                    attempt,
                    err,
                    backoff.as_secs()
                )
            },
        );
        sleep(Duration::from_millis(RPC_STAKE_ACCOUNTS_FETCH_BACKOFF_MS));
        scanned
    }
}

fn fetch_stake_account_totals(
    rpc_client: &RpcClient,
    rpc_timeout: Duration,
    withdraw_authorities: HashSet<(String, String)>,
    epoch: Epoch,
    stake_history: &StakeHistory,
    rpc_attempts: usize,
    allow_skipped_stake_pages: bool,
) -> anyhow::Result<HashMap<String, StakeAccountTotals>> {
    let started = Instant::now();
    let rpc = RefCell::new(StakeAccountsRpc {
        http: reqwest::blocking::Client::builder().build()?,
        rpc_url: rpc_client.url(),
        commitment: rpc_client.commitment(),
        rpc_attempts,
        rpc_timeout,
        requests: 0,
    });
    let add = |slice_totals: &mut SliceTotals, stake_fields: Vec<u8>| {
        slice_totals.add(&stake_fields, &withdraw_authorities, epoch, stake_history)
    };
    let count = |counts: &mut KeyCounts, key: Vec<u8>| {
        *counts.entry(key).or_default() += 1;
        Ok(())
    };
    let mut totals: HashMap<String, StakeAccountTotals> = HashMap::default();
    let skipped = walk_stake_account_slices(
        MAX_STAKE_ACCOUNTS_PER_FETCH,
        |slice| {
            rpc.borrow_mut()
                .scan(slice, STAKE_FIELDS, SliceTotals::default, add)
        },
        |slice, range| rpc.borrow_mut().scan(slice, range, KeyCounts::new, count),
        |slice, slice_totals| {
            info!(
                "Processed {} self stakes on page {slice}",
                slice_totals.self_stakes
            );
            slice_totals.merge_into(&mut totals);
        },
    )?;
    info!(
        "Scanned stake accounts with {} getProgramAccounts requests in {:?}",
        rpc.borrow().requests,
        started.elapsed()
    );
    check_skipped_slices(&skipped, allow_skipped_stake_pages)?;

    Ok(totals)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commission_falls_back_to_the_legacy_percent_field() {
        let mut stats = CommissionStats::default();
        assert_eq!(
            resolve_commission_percent(Some(5), None, &mut stats),
            Some(5)
        );
        assert_eq!(stats.from_commission, 1);
        assert_eq!(stats.from_bps, 0);
    }

    #[test]
    fn commission_falls_back_to_bps_once_percent_is_nulled() {
        let mut stats = CommissionStats::default();
        assert_eq!(
            resolve_commission_percent(None, Some(300), &mut stats),
            Some(3)
        );
        assert_eq!(stats.from_bps, 1);
        assert_eq!(stats.lossy, 0);
    }

    #[test]
    fn commission_rounds_bps_up_to_a_whole_percent_and_counts_it_lossy() {
        let mut stats = CommissionStats::default();
        assert_eq!(
            resolve_commission_percent(None, Some(250), &mut stats),
            Some(3)
        );
        assert_eq!(
            resolve_commission_percent(None, Some(249), &mut stats),
            Some(3)
        );
        assert_eq!(stats.lossy, 2);
    }

    #[test]
    fn commission_just_over_the_eligibility_cap_does_not_round_down_onto_it() {
        let mut stats = CommissionStats::default();
        assert_eq!(
            resolve_commission_percent(None, Some(1000), &mut stats),
            Some(10)
        );
        assert_eq!(
            resolve_commission_percent(None, Some(1001), &mut stats),
            Some(11)
        );
        assert_eq!(
            resolve_commission_percent(None, Some(1049), &mut stats),
            Some(11)
        );
    }

    #[test]
    fn commission_clamps_bps_beyond_the_full_percent_range() {
        let mut stats = CommissionStats::default();
        assert_eq!(
            resolve_commission_percent(None, Some(10_000), &mut stats),
            Some(100)
        );
        assert_eq!(
            resolve_commission_percent(None, Some(25_600), &mut stats),
            Some(100)
        );
        assert_eq!(
            resolve_commission_percent(None, Some(u16::MAX), &mut stats),
            Some(100)
        );
    }

    #[test]
    fn commission_prefers_bps_and_flags_disagreement_with_the_legacy_field() {
        let mut stats = CommissionStats::default();
        assert_eq!(
            resolve_commission_percent(Some(3), Some(700), &mut stats),
            Some(7)
        );
        assert_eq!(stats.disagree, 1);
        assert_eq!(stats.from_bps, 1);
        assert_eq!(stats.from_commission, 0);

        assert_eq!(
            resolve_commission_percent(Some(3), Some(300), &mut stats),
            Some(3)
        );
        assert_eq!(stats.disagree, 1);
    }

    #[test]
    fn commission_does_not_flag_a_fractional_bps_matching_its_projected_percent() {
        let mut stats = CommissionStats::default();
        assert_eq!(
            resolve_commission_percent(Some(3), Some(240), &mut stats),
            Some(3)
        );
        assert_eq!(stats.disagree, 0);
    }

    #[test]
    fn commission_treats_zero_as_resolved_not_missing() {
        let mut stats = CommissionStats::default();
        assert_eq!(
            resolve_commission_percent(Some(0), None, &mut stats),
            Some(0)
        );
        assert_eq!(
            resolve_commission_percent(None, Some(0), &mut stats),
            Some(0)
        );
        assert_eq!(stats.unresolved, 0);
    }

    #[test]
    fn commission_is_unresolved_when_the_rpc_serves_neither_field() {
        let mut stats = CommissionStats::default();
        assert_eq!(resolve_commission_percent(None, None, &mut stats), None);
        assert_eq!(stats.unresolved, 1);
    }

    #[test]
    fn inflation_reward_deserializes_the_post_simd_0291_payload() {
        let reward: RpcInflationRewardExt = serde_json::from_str(
            r#"{"amount":591366523,"commission":null,"commissionBps":300,"effectiveSlot":434592000,"epoch":1005,"postBalance":10012207545}"#,
        )
        .unwrap();
        assert_eq!(reward.commission, None);
        assert_eq!(reward.commission_bps, Some(300));

        let legacy: RpcInflationRewardExt = serde_json::from_str(
            r#"{"amount":530714233,"commission":3,"effectiveSlot":432864000,"epoch":1001,"postBalance":7729378465}"#,
        )
        .unwrap();
        assert_eq!(legacy.commission, Some(3));
        assert_eq!(legacy.commission_bps, None);
    }

    #[test]
    fn plausible_node_versions() {
        assert!(is_plausible_node_version("4.1.0"));
        assert!(is_plausible_node_version("4.1.0-rc.1"));
        assert!(is_plausible_node_version("4.2.0-beta.0"));
        assert!(is_plausible_node_version("0.505.20216"));
        assert!(!is_plausible_node_version(""));
        assert!(!is_plausible_node_version("unknown"));
        assert!(!is_plausible_node_version("4.1"));
        assert!(!is_plausible_node_version("v4.1.0"));
        assert!(!is_plausible_node_version("4.x.0"));
        assert!(!is_plausible_node_version(".."));
        assert!(!is_plausible_node_version("4.1.0garbage"));
        assert!(!is_plausible_node_version("4.1.0/extra"));
        assert!(!is_plausible_node_version("4.1.0-"));
        assert!(!is_plausible_node_version("4.1.0-rc/1"));
        assert!(!is_plausible_node_version("4.1.0.1"));
    }

    // Guards the vendored registry against upstream additions: a new client-ids.csv row with no
    // grouping arm fails here instead of silently becoming an unclassified validator.
    #[test]
    fn every_registered_client_id_has_groupings() {
        assert!(!client_registry().names.is_empty());
        for (id, name) in client_registry().names.iter() {
            assert!(
                ClientId::Registered(*id).groupings().is_some(),
                "client id {id} is in client-ids.csv but has no vendor/lineage mapping"
            );
            assert!(
                !name.is_empty(),
                "client id {id} has an empty name column in client-ids.csv"
            );
            assert_eq!(
                resolve_client_id(Some(name)).number(),
                Some(*id),
                "client id {id} does not resolve back from its own registry name {name}"
            );
        }
    }

    #[test]
    fn client_name_is_the_registry_name() {
        assert_eq!(
            resolve_client_id(Some("Unknown(8)")).name(),
            Some("Rakurai")
        );
        assert_eq!(resolve_client_id(Some("Rakurai")).name(), Some("Rakurai"));
        assert_eq!(
            resolve_client_id(Some("AgaveBam")).name(),
            Some("Agave Bam")
        );
        assert_eq!(
            resolve_client_id(Some("JitoLabs")).name(),
            Some("Jito Labs")
        );
    }

    #[test]
    fn client_label_pairs_lineage_with_the_vendor_modification() {
        let label = |raw: &str| resolve_client_id(Some(raw)).label(None);
        let some = |label: &str| Some(label.to_string());
        assert_eq!(label("Agave"), some("Agave"));
        assert_eq!(label("Solana Labs"), some("Agave"));
        assert_eq!(label("JitoLabs"), some("Agave + Jito"));
        assert_eq!(label("AgaveBam"), some("Agave + JitoBAM"));
        assert_eq!(label("AgavePaladin"), some("Agave + Paladin"));
        assert_eq!(label("Unknown(8)"), some("Agave + Rakurai"));
        assert_eq!(label("Unknown(10)"), some("Agave + Harmonic"));
        assert_eq!(label("Raiku"), some("Agave + Raiku"));
        assert_eq!(label("Frankendancer"), some("Frankendancer"));
        assert_eq!(label("Unknown(11)"), some("Frankendancer + Harmonic"));
        assert_eq!(label("Unknown(12)"), some("Firedancer + JitoBAM"));
        assert_eq!(label("FireBAM"), some("Firedancer + JitoBAM"));
        assert_eq!(label("Unknown(14)"), some("Frankendancer + JitoBAM"));
        assert_eq!(label("FrankenBAM"), some("Frankendancer + JitoBAM"));
        assert_eq!(label("Firedancer"), some("Firedancer"));
        assert_eq!(label("Unknown(9)"), some("Firedancer + Harmonic"));
        assert_eq!(label("Sig"), some("Sig"));
        assert_eq!(label("Unknown(86)"), None);
        assert_eq!(resolve_client_id(None).label(None), None);
    }

    // The label repeats the lineage as display text, so a mapping edit that touches one and not the
    // other fails here instead of serving a label that contradicts client_lineage.
    #[test]
    fn every_label_starts_with_its_own_lineage() {
        for id in client_registry().names.keys() {
            let client = ClientId::Registered(*id);
            for version in [None, Some("0.1204.40300"), Some("26.9.4")] {
                let (lineage, label) = (
                    client.lineage(version).unwrap(),
                    client.label(version).unwrap(),
                );
                let label = label.as_str();
                let mut expected = lineage.to_string();
                expected[..1].make_ascii_uppercase();
                assert!(
                    label.starts_with(&expected),
                    "client id {id} label {label} does not start with its lineage {lineage}"
                );
            }
        }
    }

    #[test]
    fn client_number_is_set_only_for_a_registered_id() {
        assert_eq!(resolve_client_id(Some("Unknown(8)")).number(), Some(8));
        assert_eq!(resolve_client_id(Some("Rakurai")).number(), Some(8));
        assert_eq!(resolve_client_id(Some("Unknown(86)")).number(), None);
        assert_eq!(resolve_client_id(Some("Unknown(86)")).name(), None);
        assert_eq!(resolve_client_id(Some("brand-new/1.0")).number(), None);
        assert_eq!(resolve_client_id(None).number(), None);
        assert_eq!(resolve_client_id(None).name(), None);
    }

    // A client the registry predates must store the same identity through either rendering, or the
    // stored id flips with whichever RPC answered and store logs a client change that never happened.
    // Id 86 is live on mainnet (Vexor), and the Foundation has not assigned it a registry entry.
    #[test]
    fn an_unregistered_client_stores_the_same_identity_whichever_form_the_rpc_renders() {
        let stored = |raw| {
            let resolved = resolve_client_id(Some(raw));
            (resolved.number(), resolved.vendor(), resolved.lineage(None))
        };
        assert_eq!(stored("Unknown(86)"), (None, None, None));
        assert_eq!(stored("Vexor"), stored("Unknown(86)"));

        // A registered id stays fully classified through either rendering.
        assert_eq!(
            stored("Unknown(8)"),
            (Some(8), Some("rakurai"), Some("agave"))
        );
        assert_eq!(stored("Rakurai"), stored("Unknown(8)"));
    }

    #[test]
    fn unclassified_clients_are_summarised_by_node_count() {
        let renderings = HashMap::from([
            ("Raiku2".to_string(), 12),
            ("Unknown(99)".to_string(), 37),
            ("Vexor".to_string(), 12),
        ]);
        assert_eq!(
            unclassified_clients_summary(&renderings),
            "Unknown(99) on 37 node(s), Raiku2 on 12 node(s), Vexor on 12 node(s)"
        );
        assert_eq!(unclassified_clients_summary(&HashMap::new()), "");
    }

    #[test]
    fn resolves_names_rendered_by_agave() {
        assert_eq!(resolve_client_id(Some("Agave")), ClientId::Registered(3));
        assert_eq!(resolve_client_id(Some("JitoLabs")), ClientId::Registered(1));
        assert_eq!(resolve_client_id(Some("AgaveBam")), ClientId::Registered(6));
        assert_eq!(
            resolve_client_id(Some("Frankendancer")),
            ClientId::Registered(2)
        );
        assert_eq!(
            resolve_client_id(Some("Firedancer")),
            ClientId::Registered(5)
        );
    }

    #[test]
    fn resolves_unknown_number_through_the_registry() {
        assert_eq!(
            resolve_client_id(Some("Unknown(8)")),
            ClientId::Registered(8)
        );
        assert_eq!(
            resolve_client_id(Some("Unknown(11)")),
            ClientId::Registered(11)
        );
        assert_eq!(
            resolve_client_id(Some("Unknown(8)")).vendor(),
            Some("rakurai")
        );
    }

    #[test]
    fn same_validator_resolves_identically_whichever_form_the_rpc_renders() {
        assert_eq!(
            resolve_client_id(Some("Unknown(6)")),
            resolve_client_id(Some("AgaveBam"))
        );
        assert_eq!(
            resolve_client_id(Some("Unknown(1)")),
            resolve_client_id(Some("Jito Labs"))
        );
    }

    #[test]
    fn vendor_groups_harmonic_across_lineages() {
        for id in [9, 10, 11] {
            assert_eq!(ClientId::Registered(id).vendor(), Some("harmonic"));
        }
        assert_eq!(ClientId::Registered(9).lineage(None), Some("firedancer"));
        assert_eq!(ClientId::Registered(10).lineage(None), Some("agave"));
        assert_eq!(
            ClientId::Registered(11).lineage(None),
            Some("frankendancer")
        );
    }

    #[test]
    fn vendor_separates_bam_from_plain_agave() {
        assert_eq!(ClientId::Registered(6).vendor(), Some("bam"));
        assert_eq!(ClientId::Registered(6).lineage(None), Some("agave"));
        assert_eq!(ClientId::Registered(3).vendor(), Some("agave"));
        assert_eq!(ClientId::Registered(12).vendor(), Some("bam"));
        assert_eq!(ClientId::Registered(12).lineage(None), Some("firedancer"));
        assert_eq!(ClientId::Registered(14).name(), Some("FrankenBAM"));
        assert_eq!(ClientId::Registered(14).vendor(), Some("bam"));
        assert_eq!(ClientId::Registered(14).engine(), Some("JitoBAM"));
        assert_eq!(
            ClientId::Registered(14).lineage(None),
            Some("frankendancer")
        );
    }

    #[test]
    fn id_12_on_a_frankendancer_version_keeps_the_frankendancer_lineage() {
        let fire_bam = ClientId::Registered(12);
        assert_eq!(
            fire_bam.lineage(Some("0.1204.40300")),
            Some("frankendancer")
        );
        assert_eq!(
            fire_bam.label(Some("0.1204.40300")),
            Some("Frankendancer + JitoBAM".to_string())
        );
        assert_eq!(fire_bam.lineage(Some("26.9.4")), Some("firedancer"));
        assert_eq!(
            fire_bam.label(Some("26.9.4")),
            Some("Firedancer + JitoBAM".to_string())
        );
        assert_eq!(fire_bam.lineage(None), Some("firedancer"));
        assert_eq!(
            fire_bam.lineage(Some("not-a-version")),
            Some("firedancer"),
            "an unparseable version reads as no version"
        );
        assert_eq!(fire_bam.vendor(), Some("bam"));
        assert_eq!(fire_bam.engine(), Some("JitoBAM"));

        assert_eq!(
            ClientId::Registered(5).lineage(Some("0.1204.40300")),
            Some("firedancer"),
            "the version rule is id 12's alone"
        );
        assert_eq!(
            ClientId::Registered(14).lineage(Some("26.9.4")),
            Some("frankendancer")
        );
    }

    // Only about what the rendering can be parsed into: the number is kept in the resolution so the
    // two unclassified shapes stay distinguishable, but `number()` deliberately does not expose it.
    #[test]
    fn unregistered_number_keeps_the_number() {
        assert_eq!(
            resolve_client_id(Some("Unknown(86)")),
            ClientId::Unrecognized(Some(86))
        );
        assert_eq!(
            resolve_client_id(Some("Unknown(37013)")),
            ClientId::Unrecognized(Some(37013))
        );
        assert_eq!(resolve_client_id(Some("Unknown(86)")).vendor(), None);
        assert_eq!(resolve_client_id(Some("Unknown(86)")).lineage(None), None);
    }

    #[test]
    fn unknown_name_has_no_recoverable_number() {
        assert_eq!(
            resolve_client_id(Some("brand-new-client/1.0")),
            ClientId::Unrecognized(None)
        );
    }

    #[test]
    fn absent_client_id_is_missing() {
        assert_eq!(resolve_client_id(None), ClientId::Missing);
        assert_eq!(resolve_client_id(Some("   ")), ClientId::Missing);
        assert_eq!(resolve_client_id(None).vendor(), None);
    }

    #[test]
    fn parse_socket_addr_ipv4() {
        assert_eq!(
            parse_socket_addr("10.0.0.1:8001"),
            Some(("10.0.0.1".to_string(), 8001))
        );
    }

    #[test]
    fn parse_socket_addr_ipv6() {
        assert_eq!(
            parse_socket_addr("[2001:db8::1]:8001"),
            Some(("2001:db8::1".to_string(), 8001))
        );
    }
}

#[cfg(test)]
mod vote_state_tests {
    use super::*;

    const NODE: [u8; 32] = [1; 32];
    const WITHDRAWER: [u8; 32] = [2; 32];
    const INFLATION_COLLECTOR: [u8; 32] = [3; 32];
    const BLOCK_COLLECTOR: [u8; 32] = [4; 32];

    // By hand because the pinned solana-client predates VoteStateV4; this pins agave's frame_v4.rs.
    fn pre_v4_account(version: u32, commission: u8, total_len: usize) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&version.to_le_bytes());
        data.extend_from_slice(&NODE);
        data.extend_from_slice(&WITHDRAWER);
        data.push(commission);
        data.resize(total_len.max(data.len()), 0);
        data
    }

    fn v4_account(
        inflation_bps: u16,
        block_bps: u16,
        pending_delegator_rewards: u64,
        total_len: usize,
    ) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&VOTE_STATE_VERSION_V4.to_le_bytes());
        data.extend_from_slice(&NODE);
        data.extend_from_slice(&WITHDRAWER);
        data.extend_from_slice(&INFLATION_COLLECTOR);
        data.extend_from_slice(&BLOCK_COLLECTOR);
        data.extend_from_slice(&inflation_bps.to_le_bytes());
        data.extend_from_slice(&block_bps.to_le_bytes());
        data.extend_from_slice(&pending_delegator_rewards.to_le_bytes());
        data.resize(total_len.max(data.len()), 0);
        data
    }

    #[test]
    fn v4_field_offsets_match_the_agave_frame() {
        let state = parse_vote_state(&v4_account(733, 1234, 42, 3762)).unwrap();
        assert_eq!(
            state,
            VoteStateFields {
                authorized_withdrawer: Pubkey::new_from_array(WITHDRAWER),
                inflation_rewards_commission_bps: Some(733),
                inflation_rewards_commission_bps_is_v4: Some(true),
                inflation_rewards_collector: Some(Pubkey::new_from_array(INFLATION_COLLECTOR)),
                block_revenue_collector: Some(Pubkey::new_from_array(BLOCK_COLLECTOR)),
                block_revenue_commission_bps: Some(1234),
                pending_delegator_rewards: Some(42),
                bls_pubkey: None,
                lamports: None,
            }
        );
    }

    #[test]
    fn a_v4_state_reads_the_bls_key_after_the_pending_rewards() {
        let mut data = v4_account(733, 1234, 42, 0);
        data.push(1);
        data.extend_from_slice(&[7; BLS_PUBKEY_COMPRESSED_SIZE]);
        data.resize(3762, 0);
        let state = parse_vote_state(&data).unwrap();
        assert_eq!(state.bls_pubkey, Some([7; BLS_PUBKEY_COMPRESSED_SIZE]));
        assert_eq!(state.pending_delegator_rewards, Some(42));

        data[VOTE_V4_BLS_PUBKEY_COMPRESSED_OFFSET] = 2;
        assert!(parse_vote_state(&data).is_err());
    }

    #[test]
    fn a_pre_v4_state_yields_no_collector_rather_than_a_zeroed_pubkey() {
        for version in [VOTE_STATE_VERSION_V1_14_11, VOTE_STATE_VERSION_V3] {
            let state = parse_vote_state(&pre_v4_account(version, 7, 3762)).unwrap();
            assert_eq!(
                state.authorized_withdrawer,
                Pubkey::new_from_array(WITHDRAWER),
                "the withdrawer sits at the same offset on every supported version"
            );
            assert_eq!(state.inflation_rewards_collector, None);
            assert_eq!(state.block_revenue_collector, None);
            assert_eq!(state.block_revenue_commission_bps, None);
            assert_eq!(state.pending_delegator_rewards, None);
            assert_ne!(
                state.inflation_rewards_collector,
                Some(Pubkey::default()),
                "absence must not read as the system program"
            );
        }
    }

    #[test]
    fn a_pre_v4_commission_projects_to_basis_points_and_says_it_is_not_v4() {
        let state = parse_vote_state(&pre_v4_account(VOTE_STATE_VERSION_V3, 7, 3762)).unwrap();
        assert_eq!(state.inflation_rewards_commission_bps, Some(700));
        assert_eq!(state.inflation_rewards_commission_bps_is_v4, Some(false));
    }

    // A commission byte above 100 is invalid on chain; agave saturates rather than overflows.
    #[test]
    fn a_pre_v4_commission_beyond_the_full_range_saturates() {
        let state =
            parse_vote_state(&pre_v4_account(VOTE_STATE_VERSION_V3, u8::MAX, 3762)).unwrap();
        assert_eq!(state.inflation_rewards_commission_bps, Some(25_500));
    }

    #[test]
    fn a_v4_state_carries_the_basis_points_a_percent_cannot_express() {
        let state = parse_vote_state(&v4_account(749, 10_000, 0, 3762)).unwrap();
        assert_eq!(state.inflation_rewards_commission_bps, Some(749));
        assert_eq!(state.inflation_rewards_commission_bps_is_v4, Some(true));
        assert_eq!(
            state.block_revenue_commission_bps,
            Some(10_000),
            "the migration default keeps all block revenue with the validator"
        );
    }

    #[test]
    fn a_truncated_account_errors_instead_of_reading_a_zeroed_field() {
        let v4 = v4_account(733, 1234, 42, 144);
        for len in 0..v4.len() {
            assert!(
                parse_vote_state(&v4[..len]).is_err(),
                "v4 truncated to {len} bytes must not parse"
            );
        }
        let pre_v4 = pre_v4_account(VOTE_STATE_VERSION_V3, 7, 69);
        for len in 0..pre_v4.len() {
            assert!(
                parse_vote_state(&pre_v4[..len]).is_err(),
                "v3 truncated to {len} bytes must not parse"
            );
        }
    }

    #[test]
    fn an_uninitialized_version_errors_rather_than_guessing_offsets() {
        assert!(
            parse_vote_state(&pre_v4_account(VOTE_STATE_VERSION_UNINITIALIZED, 7, 3762)).is_err(),
            "version 0 holds no withdrawer where every later version puts one"
        );
    }

    // Self stake reads the withdrawer alone, so a version bump must not drop the account.
    #[test]
    fn an_unknown_version_yields_the_withdrawer_and_no_commission() {
        for version in [4u32, u32::MAX] {
            let state = parse_vote_state(&pre_v4_account(version, 7, 3762)).unwrap();
            assert_eq!(
                state.authorized_withdrawer,
                Pubkey::new_from_array(WITHDRAWER),
                "version {version} must still resolve self stake"
            );
            assert_eq!(
                state.inflation_rewards_commission_bps, None,
                "version {version} must not be read at v1/v3 offsets"
            );
            assert_eq!(state.inflation_rewards_commission_bps_is_v4, None);
            assert_eq!(state.inflation_rewards_collector, None);
            assert_eq!(state.block_revenue_collector, None);
            assert_eq!(state.block_revenue_commission_bps, None);
            assert_eq!(state.pending_delegator_rewards, None);
        }
    }

    #[test]
    fn a_minority_of_unparsed_vote_accounts_still_yields_the_rest() {
        let good = v4_account(733, 1234, 0, 3762);
        let bad = pre_v4_account(VOTE_STATE_VERSION_UNINITIALIZED, 7, 3762);
        let mut accounts: Vec<(String, &[u8])> = (0..6)
            .map(|i| (format!("vote{i}"), good.as_slice()))
            .collect();
        accounts.push(("voteBad".to_string(), bad.as_slice()));

        let states = parse_vote_account_states(&accounts).unwrap();
        assert_eq!(states.len(), 6);
        assert!(!states.contains_key("voteBad"));
    }

    #[derive(Clone, Default)]
    struct CapturedLog(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLog {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_clean_parse_still_logs_an_unparsed_count_of_zero() {
        let captured = CapturedLog::default();
        env_logger::Builder::new()
            .filter_level(log::LevelFilter::Info)
            .target(env_logger::Target::Pipe(Box::new(captured.clone())))
            .try_init()
            .expect("no other test in this crate installs a logger");

        let good = v4_account(733, 1234, 0, 3762);
        let accounts: Vec<(String, &[u8])> = (0..3)
            .map(|i| (format!("voteClean{i}"), good.as_slice()))
            .collect();
        parse_vote_account_states(&accounts).unwrap();

        let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert!(
            log.contains("Parsed 3 vote accounts, 3 on vote state v4, unparsed 0"),
            "the zero is the baseline, so it must be logged: {log}"
        );
    }

    #[test]
    fn a_majority_of_unparsed_vote_accounts_fails_the_run() {
        let good = v4_account(733, 1234, 0, 3762);
        let bad = pre_v4_account(VOTE_STATE_VERSION_UNINITIALIZED, 7, 3762);
        let mut accounts: Vec<(String, &[u8])> = (0..6)
            .map(|i| (format!("voteBad{i}"), bad.as_slice()))
            .collect();
        accounts.push(("voteGood".to_string(), good.as_slice()));

        assert!(
            parse_vote_account_states(&accounts).is_err(),
            "a layout change must fail the run, not write zero self stake for the fleet"
        );
    }

    #[test]
    fn withdraw_authorities_pairs_every_parsed_account_with_its_withdrawer() {
        let states = HashMap::from_iter([
            (
                "voteA".to_string(),
                parse_vote_state(&v4_account(733, 1234, 0, 3762)).unwrap(),
            ),
            (
                "voteB".to_string(),
                parse_vote_state(&pre_v4_account(VOTE_STATE_VERSION_V3, 7, 3762)).unwrap(),
            ),
        ]);
        let withdrawer = Pubkey::new_from_array(WITHDRAWER).to_string();
        assert_eq!(
            withdraw_authorities(&states),
            HashSet::from_iter([
                (withdrawer.clone(), "voteA".to_string()),
                (withdrawer, "voteB".to_string()),
            ])
        );
    }
}

#[cfg(test)]
mod stake_account_tests {
    use super::*;
    use solana_stake_interface::state::StakeStateV2;

    const WITHDRAWER: [u8; 32] = [2; 32];
    const EPOCH: Epoch = 900;
    const VOTE: [u8; 32] = [7; 32];
    const OTHER_WITHDRAWER: [u8; 32] = [8; 32];

    fn stake_account(
        withdrawer: [u8; 32],
        stake: u64,
        activation_epoch: Epoch,
        deactivation_epoch: Epoch,
    ) -> (Pubkey, Account) {
        #[allow(deprecated)]
        let state = StakeStateV2::Stake(
            stake::state::Meta {
                rent_exempt_reserve: 0,
                authorized: stake::state::Authorized {
                    staker: Pubkey::new_from_array(withdrawer),
                    withdrawer: Pubkey::new_from_array(withdrawer),
                },
                lockup: Default::default(),
            },
            stake::state::Stake {
                delegation: stake::state::Delegation {
                    voter_pubkey: Pubkey::new_from_array(VOTE),
                    stake,
                    activation_epoch,
                    deactivation_epoch,
                    _reserved: 0.25_f64.to_le_bytes(),
                },
                credits_observed: 0,
            },
            stake::stake_flags::StakeFlags::empty(),
        );
        let mut account = Account {
            data: bincode::serialize(&state).unwrap(),
            owner: stake::program::ID,
            ..Default::default()
        };
        account.data.resize(200, 0);
        (Pubkey::new_unique(), account)
    }

    fn self_stake_authorities() -> HashSet<(String, String)> {
        HashSet::from_iter([(
            Pubkey::new_from_array(WITHDRAWER).to_string(),
            Pubkey::new_from_array(VOTE).to_string(),
        )])
    }

    // An empty history sends every settled account down the "dropped out of history" path, which
    // reports the delegation as fully effective. No path below reads an entry.
    fn process(accounts: Vec<(Pubkey, Account)>) -> (HashMap<String, StakeAccountTotals>, u64) {
        let mut slice_totals = SliceTotals::default();
        for (_, account) in accounts {
            let fields =
                &account.data[STAKE_FIELDS.offset..STAKE_FIELDS.offset + STAKE_FIELDS.length];
            slice_totals
                .add(
                    fields,
                    &self_stake_authorities(),
                    EPOCH,
                    &StakeHistory::default(),
                )
                .unwrap();
        }
        let mut totals = HashMap::default();
        let assigned = slice_totals.self_stakes;
        slice_totals.merge_into(&mut totals);
        (totals, assigned)
    }

    fn vote_totals(totals: &HashMap<String, StakeAccountTotals>) -> StakeAccountTotals {
        *totals
            .get(&Pubkey::new_from_array(VOTE).to_string())
            .expect("no entry for the vote account")
    }

    #[test]
    fn a_third_party_account_activating_lands_in_the_totals_without_self_stake() {
        let (totals, assigned) =
            process(vec![stake_account(OTHER_WITHDRAWER, 500, EPOCH, u64::MAX)]);

        let entry = vote_totals(&totals);
        assert_eq!(entry.activating, 500);
        assert_eq!(entry.deactivating, 0);
        assert_eq!(entry.self_stake, 0);
        assert_eq!(assigned, 0);
    }

    #[test]
    fn a_settled_third_party_account_adds_no_entry_at_all() {
        let (totals, assigned) = process(vec![stake_account(OTHER_WITHDRAWER, 500, 0, u64::MAX)]);

        assert!(totals.is_empty());
        assert_eq!(assigned, 0);
    }

    #[test]
    fn a_self_stake_account_deactivating_counts_on_both_sides() {
        let (totals, assigned) = process(vec![stake_account(WITHDRAWER, 500, 0, EPOCH)]);

        let entry = vote_totals(&totals);
        assert_eq!(entry.self_stake, 500);
        assert_eq!(entry.deactivating, 500);
        assert_eq!(entry.activating, 0);
        assert_eq!(assigned, 1);
    }

    #[test]
    fn a_self_stake_account_still_activating_counts_as_pending_only() {
        let (totals, assigned) = process(vec![stake_account(WITHDRAWER, 500, EPOCH, u64::MAX)]);

        let entry = vote_totals(&totals);
        assert_eq!(entry.activating, 500);
        assert_eq!(
            entry.self_stake, 0,
            "zero effective stake keeps it out of the self stake sum"
        );
        assert_eq!(assigned, 0);
    }

    #[test]
    fn the_totals_of_one_vote_account_add_up_over_several_accounts() {
        let (totals, assigned) = process(vec![
            stake_account(WITHDRAWER, 500, 0, u64::MAX),
            stake_account(OTHER_WITHDRAWER, 300, EPOCH, u64::MAX),
            stake_account(OTHER_WITHDRAWER, 200, 0, EPOCH),
            stake_account(OTHER_WITHDRAWER, 900, 0, u64::MAX),
        ]);

        let entry = vote_totals(&totals);
        assert_eq!(entry.self_stake, 500);
        assert_eq!(entry.activating, 300);
        assert_eq!(entry.deactivating, 200);
        assert_eq!(assigned, 1);
    }
}

#[cfg(test)]
mod epoch_credits_tests {
    use super::*;

    const M: u64 = u64::MAX;

    fn vote_account(history: &[EpochCreditsEntry], activated_stake: u64) -> RpcVoteAccountInfo {
        RpcVoteAccountInfo {
            vote_pubkey: String::new(),
            node_pubkey: String::new(),
            activated_stake,
            commission: 0,
            inflation_rewards_commission_bps: None,
            epoch_vote_account: true,
            epoch_credits: history.to_vec(),
            last_vote: 0,
            root_slot: 0,
        }
    }

    // testnet KmCRTozzcAXvFEH2xakNMV7GeWyftyVFKS32XGV4spW, on-chain vote state at epoch 1047
    fn testnet_history() -> Vec<EpochCreditsEntry> {
        vec![
            (1040, 803969051, 797204500),
            (1041, 810676609, 803969051),
            (1042, 810741247, 810676609),
            (M, M, M),
            (1042, 291696003588, 810741247),
            (1043, 590625720152, 291696003588),
            (1044, 899173572261, 590625720152),
        ]
    }

    #[test]
    fn tower_epoch_before_marker() {
        assert_eq!(
            split_epoch_credits(&vote_account(&testnet_history(), 1), 1041, None),
            Some(EpochCredits {
                tower_credits: Some(6707558),
                vote_reward_lamports: None,
            })
        );
    }

    #[test]
    fn migration_epoch_keeps_both_halves() {
        assert_eq!(
            split_epoch_credits(&vote_account(&testnet_history(), 1), 1042, None),
            Some(EpochCredits {
                tower_credits: Some(64638),
                vote_reward_lamports: Some(290885262341),
            })
        );
    }

    #[test]
    fn alpenglow_epoch_after_marker() {
        assert_eq!(
            split_epoch_credits(&vote_account(&testnet_history(), 1), 1043, None),
            Some(EpochCredits {
                tower_credits: None,
                vote_reward_lamports: Some(298929716564),
            })
        );
    }

    #[test]
    fn migration_epoch_with_tower_half_out_of_window() {
        let window = testnet_history()[3..].to_vec();
        assert_eq!(
            split_epoch_credits(&vote_account(&window, 1), 1042, None),
            Some(EpochCredits {
                tower_credits: None,
                vote_reward_lamports: Some(290885262341),
            })
        );
    }

    #[test]
    fn a_tower_epoch_without_votes_has_zero_credits() {
        let history = vec![(1040, 803969051, 797204500), (1042, 810741247, 810676609)];
        for epoch in [1041, 1043] {
            assert_eq!(
                split_epoch_credits(&vote_account(&history, 1), epoch, None),
                Some(EpochCredits {
                    tower_credits: Some(0),
                    vote_reward_lamports: None,
                })
            );
        }
        assert_eq!(
            split_epoch_credits(&vote_account(&[], 1), 1041, None).map(|c| c.tower_credits),
            Some(Some(0))
        );
    }

    #[test]
    fn a_staked_alpenglow_epoch_without_an_entry_earned_zero() {
        assert_eq!(
            split_epoch_credits(&vote_account(&testnet_history(), 1), 1045, None),
            Some(EpochCredits {
                tower_credits: None,
                vote_reward_lamports: Some(0),
            })
        );
    }

    #[test]
    fn an_unstaked_alpenglow_epoch_without_an_entry_is_unknown() {
        assert_eq!(
            split_epoch_credits(&vote_account(&testnet_history(), 0), 1045, None),
            None
        );
    }

    #[test]
    fn epoch_out_of_window() {
        assert_eq!(
            split_epoch_credits(&vote_account(&testnet_history(), 1), 1039, None),
            None
        );
    }

    #[test]
    fn no_marker_without_migration_epoch_is_tower() {
        let history = vec![(1045, 1217502525481, 899173572261)];
        assert_eq!(
            split_epoch_credits(&vote_account(&history, 1), 1045, None),
            Some(EpochCredits {
                tower_credits: Some(318328953220),
                vote_reward_lamports: None,
            })
        );
    }

    #[test]
    fn no_marker_uses_migration_epoch() {
        let history = vec![
            (1045, 1217502525481, 899173572261),
            (1046, 1531136696154, 1217502525481),
        ];
        assert_eq!(
            split_epoch_credits(&vote_account(&history, 1), 1045, Some(1042)),
            Some(EpochCredits {
                tower_credits: None,
                vote_reward_lamports: Some(318328953220),
            })
        );
    }

    // testnet HmA5uZ5ExST9XDsYq93QRTMoP1eVFeork5ZmBMrJhMr4, vote state at epoch 1049
    #[test]
    fn migration_epoch_without_marker_keeps_tower_credits() {
        let history = vec![
            (1041, 1231213405, 1224498690),
            (1042, 1231244005, 1231213405),
        ];
        assert_eq!(
            split_epoch_credits(&vote_account(&history, 1), 1042, Some(1042)),
            Some(EpochCredits {
                tower_credits: Some(30600),
                vote_reward_lamports: None,
            })
        );
    }

    #[test]
    fn migration_epoch_without_marker_above_tower_maximum_is_a_reward() {
        let history = vec![(1042, 291696003588, 810741247)];
        assert_eq!(
            split_epoch_credits(&vote_account(&history, 1), 1042, Some(1042)),
            Some(EpochCredits {
                tower_credits: Some(0),
                vote_reward_lamports: Some(290885262341),
            })
        );
    }

    #[test]
    fn credits_total_skips_marker() {
        assert_eq!(
            latest_credits_total(&testnet_history()[..4]),
            Some(810741247)
        );
        assert_eq!(latest_credits_total(&testnet_history()), Some(899173572261));
    }
}

#[cfg(test)]
mod collector_health_tests {
    use super::*;

    const RENT_MINIMUM: u64 = 890_880;
    const COLLECTOR: Pubkey = Pubkey::new_from_array([5; 32]);

    fn account(owner: Pubkey, lamports: u64) -> Account {
        Account {
            lamports,
            data: vec![],
            owner,
            executable: false,
            rent_epoch: 0,
        }
    }

    #[test]
    fn a_rent_exempt_system_account_is_healthy() {
        let health = evaluate_collector(
            &COLLECTOR,
            Some(&account(solana_sdk_ids::system_program::id(), RENT_MINIMUM)),
        );
        assert_eq!(
            health,
            CollectorHealth {
                owner: Some(solana_sdk_ids::system_program::id()),
                lamports: RENT_MINIMUM,
                healthy: Some(true),
            }
        );
    }

    #[test]
    fn a_funded_system_account_below_rent_is_healthy() {
        let health = evaluate_collector(
            &COLLECTOR,
            Some(&account(solana_sdk_ids::system_program::id(), 1)),
        );
        assert_eq!(
            health.healthy,
            Some(true),
            "agave skips the rent check when the collector held lamports before the payout"
        );
        assert_eq!(health.lamports, 1);
    }

    #[test]
    fn an_account_not_owned_by_the_system_program_is_unhealthy() {
        let owner = solana_stake_interface::program::id();
        let health = evaluate_collector(&COLLECTOR, Some(&account(owner, u64::MAX)));
        assert_eq!(health.healthy, Some(false));
        assert_eq!(health.owner, Some(owner));
    }

    #[test]
    fn a_missing_account_is_unknown_because_the_deposit_can_fund_it() {
        assert_eq!(
            evaluate_collector(&COLLECTOR, None),
            CollectorHealth {
                owner: None,
                lamports: 0,
                healthy: None,
            },
            "agave deposit_fees defaults a missing account and checks rent after adding the payout"
        );
    }

    #[test]
    fn the_incinerator_is_unhealthy_because_the_commission_burns() {
        assert_eq!(
            evaluate_collector(&solana_sdk_ids::incinerator::id(), None).healthy,
            Some(false),
            "agave zeroes the incinerator every block, so it reads as missing"
        );
        assert_eq!(
            evaluate_collector(
                &solana_sdk_ids::incinerator::id(),
                Some(&account(solana_sdk_ids::system_program::id(), u64::MAX)),
            )
            .healthy,
            Some(false)
        );
    }

    #[test]
    fn a_collector_equal_to_the_vote_account_is_not_checked() {
        let vote_account = COLLECTOR.to_string();
        assert_eq!(checked_collector(&vote_account, Some(COLLECTOR)), None);
        assert_eq!(checked_collector(&vote_account, None), None);
        let elsewhere = Pubkey::new_from_array([6; 32]);
        assert_eq!(
            checked_collector(&vote_account, Some(elsewhere)),
            Some(elsewhere)
        );
    }

    fn state(inflation: Option<Pubkey>, block_revenue: Option<Pubkey>) -> VoteStateFields {
        VoteStateFields {
            authorized_withdrawer: Pubkey::new_from_array([9; 32]),
            inflation_rewards_commission_bps: Some(500),
            inflation_rewards_commission_bps_is_v4: Some(true),
            inflation_rewards_collector: inflation,
            block_revenue_collector: block_revenue,
            block_revenue_commission_bps: Some(10_000),
            pending_delegator_rewards: Some(0),
            bls_pubkey: None,
            lamports: None,
        }
    }

    fn health(healthy: bool) -> CollectorHealth {
        CollectorHealth {
            owner: Some(solana_sdk_ids::system_program::id()),
            lamports: RENT_MINIMUM,
            healthy: Some(healthy),
        }
    }

    #[test]
    fn each_kind_reads_the_health_of_its_own_collector() {
        let vote_account = Pubkey::new_from_array([7; 32]);
        let inflation = Pubkey::new_from_array([1; 32]);
        let block_revenue = Pubkey::new_from_array([2; 32]);
        let health_map = HashMap::from([(inflation, health(true)), (block_revenue, health(false))]);

        let (inflation_health, block_revenue_health) = collector_health_of(
            &vote_account.to_string(),
            Some(&state(Some(inflation), Some(block_revenue))),
            &health_map,
        );
        assert_eq!(inflation_health, Some(&health(true)));
        assert_eq!(block_revenue_health, Some(&health(false)));
    }

    #[test]
    fn one_collector_shared_by_both_kinds_reads_one_entry() {
        let vote_account = Pubkey::new_from_array([7; 32]);
        let shared = Pubkey::new_from_array([3; 32]);
        let health_map = HashMap::from([(shared, health(false))]);

        let (inflation_health, block_revenue_health) = collector_health_of(
            &vote_account.to_string(),
            Some(&state(Some(shared), Some(shared))),
            &health_map,
        );
        assert_eq!(inflation_health, Some(&health(false)));
        assert_eq!(block_revenue_health, Some(&health(false)));
    }

    #[test]
    fn a_collector_equal_to_the_vote_account_or_a_missing_state_reads_no_health() {
        let vote_account = Pubkey::new_from_array([7; 32]);
        let block_revenue = Pubkey::new_from_array([2; 32]);
        let health_map =
            HashMap::from([(vote_account, health(false)), (block_revenue, health(true))]);

        let (inflation_health, block_revenue_health) = collector_health_of(
            &vote_account.to_string(),
            Some(&state(Some(vote_account), Some(block_revenue))),
            &health_map,
        );
        assert_eq!(
            inflation_health, None,
            "agave pays the vote account unchecked, so its entry must not be read"
        );
        assert_eq!(block_revenue_health, Some(&health(true)));

        assert_eq!(
            collector_health_of(&vote_account.to_string(), None, &health_map),
            (None, None)
        );
    }
}

#[cfg(test)]
mod validator_info_tests {
    use super::*;

    fn config_account(keys: Vec<(Pubkey, bool)>, info: &str) -> Account {
        let mut data = bincode::serialize(&ConfigKeys { keys }).unwrap();
        data.extend(bincode::serialize(info).unwrap());
        Account {
            lamports: 1,
            data,
            owner: solana_config_interface::id(),
            executable: false,
            rent_epoch: 0,
        }
    }

    #[test]
    fn a_key_list_holding_only_the_validator_info_id_is_rejected() {
        let account = config_account(vec![(validator_info::id(), false)], r#"{"name":"x"}"#);
        assert!(parse_validator_info(&Pubkey::new_unique(), &account).is_err());
    }

    #[test]
    fn a_validator_info_account_yields_its_signer_and_name() {
        let validator = Pubkey::new_unique();
        let account = config_account(
            vec![(validator_info::id(), false), (validator, true)],
            r#"{"name":"x"}"#,
        );
        let (pubkey, info) = parse_validator_info(&Pubkey::new_unique(), &account).unwrap();
        assert_eq!(pubkey, validator);
        assert_eq!(info.name.as_deref(), Some("x"));
    }
}

#[cfg(test)]
mod stake_accounts_slice_tests {
    use super::*;
    use solana_account_decoder::encode_ui_account;
    use solana_rpc_client_api::response::RpcKeyedAccount;
    use solana_stake_interface::stake_flags::StakeFlags;
    use solana_stake_interface::state::StakeStateV2;
    use solana_stake_interface::state::{Authorized, Delegation, Lockup, Meta, Stake};

    const LIMIT: u64 = 30;

    fn key(prefix: &[u8], fill: u8) -> Pubkey {
        let mut bytes = [fill; 32];
        bytes[..prefix.len()].copy_from_slice(prefix);
        Pubkey::new_from_array(bytes)
    }

    #[allow(deprecated)]
    fn meta(withdrawer: Pubkey) -> Meta {
        Meta {
            rent_exempt_reserve: 0,
            authorized: Authorized {
                staker: Pubkey::new_unique(),
                withdrawer,
            },
            lockup: Lockup::default(),
        }
    }

    fn padded(state: StakeStateV2) -> Vec<u8> {
        let mut data = bincode::serialize(&state).unwrap();
        data.resize(200, 0);
        data
    }

    fn stake_account_data(withdrawer: Pubkey, voter: Pubkey) -> Vec<u8> {
        padded(StakeStateV2::Stake(
            meta(withdrawer),
            Stake {
                delegation: Delegation::new(&voter, 1, 0),
                credits_observed: 0,
            },
            StakeFlags::empty(),
        ))
    }

    fn initialized_account_data(withdrawer: Pubkey) -> Vec<u8> {
        padded(StakeStateV2::Initialized(meta(withdrawer)))
    }

    fn two_accounts_per_page() -> Vec<Vec<u8>> {
        (0..=u8::MAX)
            .flat_map(|page| {
                (0..2).map(move |i| stake_account_data(key(&[page, i], i), Pubkey::new_unique()))
            })
            .collect()
    }

    struct FakeRpc<'a> {
        accounts: &'a [Vec<u8>],
        stream_limit: u64,
        probe_limit: u64,
        fetches: usize,
        probes: usize,
    }

    impl FakeRpc<'_> {
        fn matching(&self, slice: &StakeAccountsSlice) -> impl Iterator<Item = &Vec<u8>> {
            let filters = slice.filters();
            self.accounts.iter().filter(move |data| {
                filters.iter().all(|filter| match filter {
                    RpcFilterType::DataSize(size) => data.len() as u64 == *size,
                    RpcFilterType::Memcmp(memcmp) => memcmp.bytes_match(data),
                    other => panic!("Unexpected filter {other:?}"),
                })
            })
        }

        fn fetch(&mut self, slice: &StakeAccountsSlice) -> anyhow::Result<SliceFetch<u64>> {
            self.fetches += 1;
            let matching = self.matching(slice).count() as u64;
            Ok(if matching > self.stream_limit {
                SliceFetch::TooLarge
            } else {
                SliceFetch::Fetched(matching)
            })
        }

        fn probe(
            &mut self,
            slice: &StakeAccountsSlice,
            range: UiDataSliceConfig,
        ) -> anyhow::Result<SliceFetch<KeyCounts>> {
            self.probes += 1;
            let mut counts = KeyCounts::new();
            for data in self.matching(slice) {
                *counts
                    .entry(data[range.offset..range.offset + range.length].to_vec())
                    .or_default() += 1;
            }
            Ok(if counts.values().sum::<u64>() > self.probe_limit {
                SliceFetch::TooLarge
            } else {
                SliceFetch::Fetched(counts)
            })
        }
    }

    struct Walk {
        fetches: usize,
        probes: usize,
        counted: u64,
        skipped: Vec<StakeAccountsSlice>,
    }

    impl Walk {
        fn requests_beyond_the_pages(&self) -> usize {
            self.fetches + self.probes - 256
        }
    }

    fn walk_with_limits(accounts: &[Vec<u8>], stream_limit: u64, probe_limit: u64) -> Walk {
        let rpc = RefCell::new(FakeRpc {
            accounts,
            stream_limit,
            probe_limit,
            fetches: 0,
            probes: 0,
        });
        let mut counted = 0;
        let skipped = walk_stake_account_slices(
            LIMIT,
            |slice| rpc.borrow_mut().fetch(slice),
            |slice, range| rpc.borrow_mut().probe(slice, range),
            |_, matching| counted += matching,
        )
        .unwrap();
        let rpc = rpc.into_inner();
        Walk {
            fetches: rpc.fetches,
            probes: rpc.probes,
            counted,
            skipped,
        }
    }

    fn walk(accounts: &[Vec<u8>]) -> Walk {
        walk_with_limits(accounts, LIMIT, u64::MAX)
    }

    #[test]
    fn filter_offsets_match_the_stake_account_layout() {
        let withdrawer = Pubkey::new_unique();
        let voter = Pubkey::new_unique();
        let data = stake_account_data(withdrawer, voter);
        assert_eq!(
            &data[WITHDRAW_AUTHORITY_OFFSET..WITHDRAW_AUTHORITY_OFFSET + 32],
            withdrawer.as_ref()
        );
        assert_eq!(&data[VOTER_OFFSET..VOTER_OFFSET + 32], voter.as_ref());
        assert_eq!(data[..4], STAKE_VARIANT_TAG);
        assert_eq!(initialized_account_data(withdrawer)[..4], [1, 0, 0, 0]);
    }

    #[test]
    fn every_page_is_fetched_once_when_none_exceeds_the_limit() {
        let accounts = two_accounts_per_page();
        let walk = walk(&accounts);
        assert_eq!((walk.fetches, walk.probes), (256, 0));
        assert_eq!(walk.counted, accounts.len() as u64);
        assert!(walk.skipped.is_empty());
    }

    #[test]
    fn undelegated_accounts_are_filtered_out_before_they_can_force_a_split() {
        let mut accounts = two_accounts_per_page();
        let withdrawer = key(&[79, 1, 2, 3, 4], 9);
        accounts.extend((0..500).map(|_| initialized_account_data(withdrawer)));
        let walk = walk(&accounts);
        assert_eq!((walk.fetches, walk.probes), (256, 0));
        assert_eq!(walk.counted, accounts.len() as u64 - 500);
        assert!(walk.skipped.is_empty());
    }

    #[test]
    fn a_page_within_the_stream_limit_is_fetched_whole_in_one_request() {
        let mut accounts = two_accounts_per_page();
        let withdrawer = key(&[79, 1, 2, 3, 4], 9);
        accounts.extend((0..1000).map(|_| stake_account_data(withdrawer, key(&[7], 7))));
        let walk = walk_with_limits(&accounts, u64::MAX, u64::MAX);
        assert_eq!((walk.fetches, walk.probes), (256, 0));
        assert_eq!(walk.counted, accounts.len() as u64);
        assert!(walk.skipped.is_empty());
    }

    #[test]
    fn a_page_shared_by_many_withdrawers_fetches_only_the_populated_next_bytes() {
        let mut accounts = two_accounts_per_page();
        accounts.extend((0..20).flat_map(|i| {
            (0..3).map(move |_| stake_account_data(key(&[79, i], 0), Pubkey::new_unique()))
        }));
        let walk = walk(&accounts);
        assert_eq!(walk.probes, 1);
        // The page's probe and the 20 populated sub-pages.
        assert_eq!(walk.requests_beyond_the_pages(), 1 + 20);
        assert_eq!(walk.counted, accounts.len() as u64);
        assert!(walk.skipped.is_empty());
    }

    #[test]
    fn one_withdrawer_spread_over_voters_takes_a_few_dozen_requests_not_a_thousand() {
        let mut accounts = two_accounts_per_page();
        let withdrawer = key(&[79, 1, 2, 3, 4], 9);
        accounts
            .extend((0..1000).map(|i| stake_account_data(withdrawer, key(&[(i % 40) as u8], 0))));
        let walk = walk(&accounts);
        // Page: probe. Its two neighbours. The withdrawer: its fetch, voter probe and its 40
        // voter sub-pages.
        assert_eq!(walk.requests_beyond_the_pages(), 1 + 2 + 2 + 40);
        assert!(walk.requests_beyond_the_pages() < 50);
        assert_eq!(walk.counted, accounts.len() as u64);
        assert!(walk.skipped.is_empty());
    }

    #[test]
    fn one_withdrawer_on_one_voter_is_skipped_and_everything_else_counted() {
        let mut accounts = two_accounts_per_page();
        let withdrawer = key(&[79, 1, 2, 3, 4], 9);
        accounts.extend((0..50).map(|_| stake_account_data(withdrawer, key(&[7], 7))));
        let walk = walk(&accounts);
        assert_eq!(walk.requests_beyond_the_pages(), 1 + 2 + 2 + 1);
        assert_eq!(walk.counted, accounts.len() as u64 - 50);
        assert_eq!(
            walk.skipped,
            vec![StakeAccountsSlice {
                withdrawer_prefix: vec![79, 1, 2, 3],
                voter_prefix: vec![7],
            }]
        );
        assert_eq!(walk.skipped[0].to_string(), "79/1/2/3 voter 7");
    }

    #[test]
    fn a_probe_over_its_own_limit_falls_back_to_splitting_into_every_next_byte() {
        let mut accounts = two_accounts_per_page();
        let withdrawer = key(&[79, 1, 2, 3, 4], 9);
        accounts.extend((0..50).map(|v| stake_account_data(withdrawer, key(&[v], v))));
        let walk = walk_with_limits(&accounts, LIMIT, LIMIT);
        assert_eq!(walk.probes, 4);
        assert_eq!(walk.fetches, 256 + 3 * 256 + 256);
        assert_eq!(walk.counted, accounts.len() as u64);
        assert!(walk.skipped.is_empty());
    }

    #[test]
    fn a_failed_fetch_names_its_page() {
        let err = walk_stake_account_slices(
            LIMIT,
            |slice| {
                if slice.withdrawer_prefix == [3] {
                    anyhow::bail!("connection reset")
                }
                Ok(SliceFetch::Fetched(()))
            },
            |_, _| unreachable!(),
            |_, _| {},
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "Failed to fetch stake accounts on page 3");
    }

    #[test]
    fn a_skipped_slice_fails_the_run_by_default() {
        let skipped = [StakeAccountsSlice {
            withdrawer_prefix: vec![79, 1, 2, 3],
            voter_prefix: vec![7],
        }];
        let err = check_skipped_slices(&skipped, false).unwrap_err();
        assert!(err.to_string().contains("79/1/2/3 voter 7"), "{err}");
    }

    #[test]
    fn a_skipped_slice_is_tolerated_when_allowed() {
        let skipped = [StakeAccountsSlice::page(79)];
        assert!(check_skipped_slices(&skipped, true).is_ok());
        assert!(check_skipped_slices(&[], false).is_ok());
    }

    fn collect_data(body: &[u8], cap: u64) -> anyhow::Result<SliceFetch<Vec<Vec<u8>>>> {
        let mut collected = vec![];
        Ok(
            match fold_rpc_accounts(body, cap, |data| {
                collected.push(data);
                Ok(())
            })? {
                SliceFetch::Fetched(()) => SliceFetch::Fetched(collected),
                SliceFetch::TooLarge => SliceFetch::TooLarge,
            },
        )
    }

    #[test]
    fn an_rpc_scan_error_reads_as_too_large() {
        let body =
            br#"{"jsonrpc":"2.0","error":{"code":-32012,"message":"page too large"},"id":1}"#;
        assert_eq!(collect_data(body, 1 << 20).unwrap(), SliceFetch::TooLarge);
    }

    #[test]
    fn other_rpc_errors_fail() {
        let body =
            br#"{"jsonrpc":"2.0","error":{"code":-32005,"message":"Node is behind"},"id":1}"#;
        assert!(collect_data(body, 1 << 20).is_err());
    }

    fn keyed_accounts_body(
        accounts: &[(Pubkey, Account)],
        data_slice: UiDataSliceConfig,
    ) -> Vec<u8> {
        let keyed: Vec<_> = accounts
            .iter()
            .map(|(pubkey, account)| RpcKeyedAccount {
                pubkey: pubkey.to_string(),
                account: encode_ui_account(
                    pubkey,
                    account,
                    UiAccountEncoding::Base64,
                    None,
                    Some(data_slice),
                ),
            })
            .collect();
        serde_json::to_vec(&json!({"jsonrpc": "2.0", "result": keyed, "id": 1})).unwrap()
    }

    fn stake_account(withdrawer: Pubkey, voter: Pubkey) -> (Pubkey, Account) {
        (
            Pubkey::new_unique(),
            Account {
                lamports: 42,
                data: stake_account_data(withdrawer, voter),
                owner: stake::program::ID,
                executable: false,
                rent_epoch: 0,
            },
        )
    }

    #[test]
    fn a_streamed_result_folds_the_stake_fields_into_totals() {
        let withdrawer = Pubkey::new_unique();
        let voter = Pubkey::new_unique();
        let accounts = vec![
            stake_account(withdrawer, voter),
            stake_account(Pubkey::new_unique(), voter),
        ];
        let body = keyed_accounts_body(&accounts, STAKE_FIELDS);
        let authorities = HashSet::from([(withdrawer.to_string(), voter.to_string())]);
        let mut slice_totals = SliceTotals::default();
        let fetched = fold_rpc_accounts(&body[..], 1 << 20, |fields| {
            slice_totals.add(&fields, &authorities, 1, &StakeHistory::default())
        })
        .unwrap();
        assert_eq!(fetched, SliceFetch::Fetched(()));
        assert_eq!(slice_totals.self_stakes, 1);
        assert_eq!(slice_totals.totals[&voter.to_string()].self_stake, 1);
    }

    #[test]
    fn a_body_over_the_cap_reads_as_too_large() {
        let accounts = vec![stake_account(Pubkey::new_unique(), Pubkey::new_unique())];
        let body = keyed_accounts_body(&accounts, STAKE_FIELDS);
        let cap = body.len() as u64;
        assert!(matches!(
            collect_data(&body, cap).unwrap(),
            SliceFetch::Fetched(_)
        ));
        assert_eq!(collect_data(&body, cap - 1).unwrap(), SliceFetch::TooLarge);
    }

    #[test]
    fn a_probe_reads_the_sliced_bytes_of_each_account() {
        let accounts: Vec<_> = [7, 7, 9]
            .iter()
            .map(|&v| stake_account(Pubkey::new_unique(), key(&[v], 0)))
            .collect();
        let range = UiDataSliceConfig {
            offset: VOTER_OFFSET,
            length: 1,
        };
        let body = keyed_accounts_body(&accounts, range);
        assert_eq!(
            collect_data(&body, 1 << 20).unwrap(),
            SliceFetch::Fetched(vec![vec![7], vec![7], vec![9]])
        );
    }
}
