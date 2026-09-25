use crate::common::retry_blocking;
use crate::common::QuadraticBackoffStrategy;
use crate::marinade_service::fetch_bonds;
use crate::validators::*;
use bincode::deserialize;
use csv::{required, Column};
use log::{info, warn};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use solana_account_decoder::validator_info;
use solana_account_decoder::UiAccountEncoding;
use solana_commitment_config::CommitmentConfig;
use solana_config_program_client::{get_config_data, ConfigKeys};
use solana_program::{
    stake_history::{StakeHistory, StakeHistoryEntry},
    sysvar::stake_history,
};
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::client_error::Error as ClientError;
use solana_rpc_client_api::config::{
    RpcAccountInfoConfig, RpcEpochConfig, RpcProgramAccountsConfig,
};
use solana_rpc_client_api::filter::{Memcmp, RpcFilterType};
use solana_rpc_client_api::request::RpcRequest;
use solana_rpc_client_api::response::RpcVoteAccountStatus;
use solana_sdk::{
    account::from_account,
    clock::{Epoch, Slot},
    slot_history::{self, SlotHistory},
    sysvar,
};
use solana_sdk::{account::Account, pubkey::Pubkey};
use solana_stake_interface::{self as stake, state::StakeStateV2};
use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    sync::OnceLock,
    thread::sleep,
    time::Duration,
};

const RPC_STAKE_ACCOUNTS_FETCH_BACKOFF_MS: u64 = 200;
const WITHDRAW_AUTHORITY_OFFSET: usize = 4 + 8 + 32;
const MAX_GET_INFLATION_REWARD_ADDRESSES: usize = 32;

pub fn solana_client(url: String, commitment: String) -> RpcClient {
    RpcClient::new_with_commitment(url, CommitmentConfig::from_str(&commitment).unwrap())
}

pub fn solana_client_with_timeout(url: String, timeout: Duration, commitment: String) -> RpcClient {
    RpcClient::new_with_timeout_and_commitment(
        url,
        timeout,
        CommitmentConfig::from_str(&commitment).unwrap(),
    )
}

pub fn get_stake_history(rpc_client: &RpcClient) -> anyhow::Result<StakeHistory> {
    Ok(bincode::deserialize(
        &rpc_client.get_account_data(&stake_history::ID)?,
    )?)
}

pub fn get_credits(vote_accounts: &RpcVoteAccountStatus, epoch: Epoch) -> HashMap<String, u64> {
    info!("Getting credits");
    let mut credits = HashMap::new();

    for vote_account in vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
    {
        for (record_epoch, end_credits, start_credits) in vote_account.epoch_credits.iter() {
            if *record_epoch == epoch {
                credits.insert(
                    vote_account.vote_pubkey.clone(),
                    end_credits - start_credits,
                );
            }
        }
    }

    credits
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

    pub fn lineage(&self) -> Option<&'static str> {
        self.groupings().map(|grouping| grouping.lineage)
    }

    pub fn engine(&self) -> Option<&'static str> {
        self.groupings().and_then(|grouping| grouping.engine)
    }

    /// The lineage as display text, and the block engine after it: `Agave + Jito`.
    pub fn label(&self) -> Option<String> {
        let grouping = self.groupings()?;
        let mut label = grouping.lineage.to_string();
        label[..1].make_ascii_uppercase();
        if let Some(engine) = grouping.engine {
            label.push_str(" + ");
            label.push_str(engine);
        }

        Some(label)
    }

    // Vendor is who ships the binary, lineage is which codebase it forks, engine is the block engine
    // the binary runs; the registry assigns a separate id per lineage variant of a vendor, so all
    // three are a function of the id alone.
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
            12 => ClientGrouping::new("bam", "frankendancer", Some("JitoBAM")),
            13 => ClientGrouping::new("raiku", "agave", Some("Raiku")),
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

pub fn get_block_production_by_validator(
    rpc_client: &RpcClient,
    epoch: Epoch,
) -> anyhow::Result<HashMap<String, (usize, usize)>> {
    info!("Getting block production by validator");
    let epoch_schedule = rpc_client.get_epoch_schedule()?;
    let first_slot_in_epoch = epoch_schedule.get_first_slot_in_epoch(epoch);
    let last_slot_in_epoch = epoch_schedule.get_last_slot_in_epoch(epoch);

    let current_epoch_production = rpc_client.get_block_production()?;
    if first_slot_in_epoch == current_epoch_production.value.range.first_slot {
        return Ok(current_epoch_production.value.by_identity);
    }

    let confirmed_blocks =
        get_confirmed_blocks(rpc_client, first_slot_in_epoch, last_slot_in_epoch)?;

    let leader_schedule = rpc_client
        .get_leader_schedule_with_commitment(
            Some(first_slot_in_epoch),
            CommitmentConfig::finalized(), // todo take from config
        )?
        .unwrap();

    let mut blocks_and_slots = HashMap::new();
    for (validator_identity, relative_slots) in leader_schedule {
        let mut validator_blocks = 0;
        let mut validator_slots = 0;
        for relative_slot in relative_slots {
            let slot = first_slot_in_epoch + relative_slot as Slot;
            validator_slots += 1;
            if confirmed_blocks.contains(&slot) {
                validator_blocks += 1;
            }
        }
        if validator_slots > 0 {
            let e = blocks_and_slots.entry(validator_identity).or_insert((0, 0));
            e.0 += validator_slots;
            e.1 += validator_blocks;
        }
    }

    Ok(blocks_and_slots)
}

fn get_confirmed_blocks(
    rpc_client: &RpcClient,
    start_slot: Slot,
    end_slot: Slot,
) -> anyhow::Result<HashSet<Slot>> {
    info!("loading slot history. slot range is [{start_slot},{end_slot}]");
    let slot_history_account = rpc_client
        .get_account_with_commitment(&sysvar::slot_history::id(), CommitmentConfig::finalized())?
        .value
        .unwrap();

    let slot_history: SlotHistory = from_account(&slot_history_account).unwrap();

    if start_slot >= slot_history.oldest() && end_slot <= slot_history.newest() {
        info!("slot range within the SlotHistory sysvar");
        Ok((start_slot..=end_slot)
            .filter(|slot| slot_history.check(*slot) == slot_history::Check::Found)
            .collect())
    } else {
        anyhow::bail!("slot range is not within the SlotHistory sysvar")
    }
}

fn parse_validator_info(
    pubkey: &Pubkey,
    account: &Account,
) -> anyhow::Result<(Pubkey, ValidatorInfo)> {
    if account.owner != solana_config_program_client::ID {
        anyhow::bail!("{pubkey} is not a validator info account");
    }
    let key_list: ConfigKeys = deserialize(&account.data)?;
    if !key_list.keys.is_empty() && key_list.keys.contains(&(validator_info::id(), false)) {
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
pub fn get_validators_info(
    rpc_client: &RpcClient,
) -> anyhow::Result<HashMap<String, ValidatorInfo>> {
    info!("Getting validator info");
    let validator_info = rpc_client.get_program_accounts(&solana_config_program_client::ID)?;

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
    let vote_program_id = solana_vote_program::id();
    let vote_accounts = rpc_client.get_program_accounts(&vote_program_id)?;
    let accounts: Vec<(String, &[u8])> = vote_accounts
        .iter()
        .map(|(account_pubkey, account)| (account_pubkey.to_string(), account.data.as_slice()))
        .collect();

    parse_vote_account_states(&accounts)
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
    info!(
        "Parsed {} vote accounts, {v4} on vote state v4",
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

pub fn get_stake_account_totals(
    rpc_client: &RpcClient,
    epoch: Epoch,
    stake_history: &StakeHistory,
    bonds_url: &str,
    allow_zero_funded_bonds: bool,
    rpc_attempts: usize,
    vote_account_states: &HashMap<String, VoteStateFields>,
) -> anyhow::Result<HashMap<String, StakeAccountTotals>> {
    let mut totals = fetch_stake_account_totals(
        rpc_client,
        withdraw_authorities(vote_account_states),
        epoch,
        stake_history,
        rpc_attempts,
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

fn fetch_stake_accounts_on_page(
    rpc_client: &RpcClient,
    page: u8,
    rpc_attempts: usize,
) -> Result<Vec<(Pubkey, Account)>, Box<ClientError>> {
    let mut filters: Vec<RpcFilterType> = vec![RpcFilterType::DataSize(200)];
    filters.push(RpcFilterType::Memcmp(Memcmp::new_raw_bytes(
        WITHDRAW_AUTHORITY_OFFSET,
        vec![page],
    )));

    let self_stakes = retry_blocking(
        || {
            rpc_client
                .get_program_accounts_with_config(
                    &stake::program::ID,
                    RpcProgramAccountsConfig {
                        filters: Some(filters.clone()),
                        account_config: RpcAccountInfoConfig {
                            encoding: Some(UiAccountEncoding::Base64),
                            commitment: Some(rpc_client.commitment()),
                            data_slice: None,
                            min_context_slot: None,
                        },
                        with_context: None,
                        sort_results: None,
                    },
                )
                .map_err(Box::new)
        },
        QuadraticBackoffStrategy::iter_durations(rpc_attempts),
        |err, attempt, backoff| {
            warn!(
                "Attempt {} has failed: {}, retrying in {:?} seconds",
                attempt,
                err,
                backoff.as_secs()
            )
        },
    )?;
    Ok(self_stakes)
}

fn process_stake_accounts(
    accounts: Vec<(Pubkey, Account)>,
    totals: &mut HashMap<String, StakeAccountTotals>,
    withdraw_authorities: &HashSet<(String, String)>,
    epoch: Epoch,
    stake_history: &StakeHistory,
) -> u64 {
    let mut self_stake_assigned = 0;
    for (_pubkey, account) in accounts.iter() {
        if let Ok(stake_account) = bincode::deserialize(&account.data) {
            if let Some((withdrawer_key, vote_key)) = get_withdrawer_and_vote_keys(&stake_account) {
                let StakeHistoryEntry {
                    effective,
                    activating,
                    deactivating,
                } = stake_account
                    .stake()
                    .unwrap()
                    .delegation
                    .stake_activating_and_deactivating(epoch, stake_history, None);
                let is_self_stake = withdraw_authorities
                    .contains(&(withdrawer_key, vote_key.clone()))
                    && effective != 0;
                if !is_self_stake && activating == 0 && deactivating == 0 {
                    continue;
                }
                let totals = totals.entry(vote_key).or_default();
                totals.activating += activating;
                totals.deactivating += deactivating;
                if is_self_stake {
                    self_stake_assigned += 1;
                    totals.self_stake += effective;
                }
            }
        }
    }

    self_stake_assigned
}

fn get_withdrawer_and_vote_keys(stake_account: &StakeStateV2) -> Option<(String, String)> {
    stake_account.delegation().and_then(|vote_account| {
        stake_account.authorized().map(|withdrawer| {
            (
                withdrawer.withdrawer.to_string(),
                vote_account.voter_pubkey.to_string(),
            )
        })
    })
}

fn fetch_stake_account_totals(
    rpc_client: &RpcClient,
    withdraw_authorities: HashSet<(String, String)>,
    epoch: Epoch,
    stake_history: &StakeHistory,
    rpc_attemtps: usize,
) -> anyhow::Result<HashMap<String, StakeAccountTotals>> {
    let mut totals: HashMap<String, StakeAccountTotals> = HashMap::default();
    for page in 0..=u8::MAX {
        match fetch_stake_accounts_on_page(rpc_client, page, rpc_attemtps) {
            Ok(accounts) => {
                let processed = process_stake_accounts(
                    accounts,
                    &mut totals,
                    &withdraw_authorities,
                    epoch,
                    stake_history,
                );
                info!("Processed {processed} self stakes on page {page}");
            }
            Err(err) => {
                panic!("Failed to fetch stake accounts on page {page}: {err}");
            }
        }

        sleep(Duration::from_millis(RPC_STAKE_ACCOUNTS_FETCH_BACKOFF_MS));
    }

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
        let label = |raw: &str| resolve_client_id(Some(raw)).label();
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
        assert_eq!(label("Unknown(12)"), some("Frankendancer + JitoBAM"));
        assert_eq!(label("Firedancer"), some("Firedancer"));
        assert_eq!(label("Unknown(9)"), some("Firedancer + Harmonic"));
        assert_eq!(label("Sig"), some("Sig"));
        assert_eq!(label("Unknown(86)"), None);
        assert_eq!(resolve_client_id(None).label(), None);
    }

    // The label repeats the lineage as display text, so a mapping edit that touches one and not the
    // other fails here instead of serving a label that contradicts client_lineage.
    #[test]
    fn every_label_starts_with_its_own_lineage() {
        for id in client_registry().names.keys() {
            let client = ClientId::Registered(*id);
            let (lineage, label) = (client.lineage().unwrap(), client.label().unwrap());
            let label = label.as_str();
            let mut expected = lineage.to_string();
            expected[..1].make_ascii_uppercase();
            assert!(
                label.starts_with(&expected),
                "client id {id} label {label} does not start with its lineage {lineage}"
            );
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
            (resolved.number(), resolved.vendor(), resolved.lineage())
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
            ("Unknown(14)".to_string(), 37),
            ("Vexor".to_string(), 12),
        ]);
        assert_eq!(
            unclassified_clients_summary(&renderings),
            "Unknown(14) on 37 node(s), Raiku2 on 12 node(s), Vexor on 12 node(s)"
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
        assert_eq!(ClientId::Registered(9).lineage(), Some("firedancer"));
        assert_eq!(ClientId::Registered(10).lineage(), Some("agave"));
        assert_eq!(ClientId::Registered(11).lineage(), Some("frankendancer"));
    }

    #[test]
    fn vendor_separates_bam_from_plain_agave() {
        assert_eq!(ClientId::Registered(6).vendor(), Some("bam"));
        assert_eq!(ClientId::Registered(6).lineage(), Some("agave"));
        assert_eq!(ClientId::Registered(3).vendor(), Some("agave"));
        assert_eq!(ClientId::Registered(12).vendor(), Some("bam"));
        assert_eq!(ClientId::Registered(12).lineage(), Some("frankendancer"));
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
        assert_eq!(resolve_client_id(Some("Unknown(86)")).lineage(), None);
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
            }
        );
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
                    warmup_cooldown_rate: 0.25,
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
        let mut totals = HashMap::default();
        let assigned = process_stake_accounts(
            accounts,
            &mut totals,
            &self_stake_authorities(),
            EPOCH,
            &StakeHistory::default(),
        );
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
