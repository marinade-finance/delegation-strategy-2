use crate::common::retry_blocking;
use crate::common::QuadraticBackoffStrategy;
use crate::marinade_service::fetch_bonds;
use crate::validators::*;
use bincode::deserialize;
use log::{info, warn};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use solana_account_decoder::validator_info;
use solana_account_decoder::UiAccountEncoding;
use solana_client::{
    client_error::ClientError,
    rpc_client::RpcClient,
    rpc_config::{RpcAccountInfoConfig, RpcEpochConfig, RpcProgramAccountsConfig},
    rpc_filter::{Memcmp, RpcFilterType},
    rpc_request::RpcRequest,
    rpc_response::RpcVoteAccountStatus,
};
use solana_commitment_config::CommitmentConfig;
use solana_config_program::{get_config_data, ConfigKeys};
use solana_program::{
    stake_history::{StakeHistory, StakeHistoryEntry},
    sysvar::stake_history,
};
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

pub fn get_credits(rpc_client: &RpcClient, epoch: Epoch) -> anyhow::Result<HashMap<String, u64>> {
    info!("Getting credits");
    let vote_accounts = rpc_client.get_vote_accounts()?;

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

    Ok(credits)
}

const CLIENT_IDS_CSV: &str = include_str!("../client-ids.csv");

struct ClientRegistry {
    names: HashMap<u16, String>,
    ids_by_name: HashMap<String, u16>,
}

fn client_registry() -> &'static ClientRegistry {
    static REGISTRY: OnceLock<ClientRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut names = HashMap::new();
        let mut ids_by_name = HashMap::new();
        let mut reader = csv::Reader::from_reader(CLIENT_IDS_CSV.as_bytes());
        for record in reader.records().flatten() {
            let (Some(id), Some(name)) = (record.get(0), record.get(1)) else {
                continue;
            };
            let Ok(id) = id.trim().parse::<u16>() else {
                continue;
            };
            ids_by_name.insert(canonical_client_name(name), id);
            names.insert(id, name.trim().to_string());
        }
        ClientRegistry { names, ids_by_name }
    })
}

// Agave renders registry names without the separators the CSV uses ("AgaveBam" vs "Agave Bam").
fn canonical_client_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
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

// The frontend renders this single field, so it stays populated even for ids our registry predates.
fn client_display_name(resolved: ClientId, raw: Option<&str>) -> Option<String> {
    resolved.name().map(str::to_string).or_else(|| {
        raw.map(str::trim)
            .filter(|raw| !raw.is_empty())
            .map(str::to_string)
    })
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
        self.groupings().map(|(vendor, _, _)| vendor)
    }

    pub fn lineage(&self) -> Option<&'static str> {
        self.groupings().map(|(_, lineage, _)| lineage)
    }

    pub fn label(&self) -> Option<&'static str> {
        self.groupings().map(|(_, _, label)| label)
    }

    // Vendor is who ships the binary, lineage is which codebase it forks, label renders the pair for
    // display; the registry assigns a separate id per lineage variant of a vendor, so all three are a
    // function of the id alone.
    fn groupings(&self) -> Option<(&'static str, &'static str, &'static str)> {
        let ClientId::Registered(id) = self else {
            return None;
        };
        // Ids 2 and 5 carry no "+ Jito": the bundle tile is a config flag in the same binary, so gossip
        // cannot tell a bundle-running node from a plain one, and neither claim is observable.
        Some(match id {
            // Id 0 is Agave's pre-rename vendor, not a fork of it, so it labels bare like id 3.
            0 => ("solana-labs", "agave", "Agave"),
            1 => ("jito", "agave", "Agave + Jito"),
            2 => ("frankendancer", "frankendancer", "Frankendancer"),
            3 => ("agave", "agave", "Agave"),
            4 => ("paladin", "agave", "Agave + Paladin"),
            5 => ("firedancer", "firedancer", "Firedancer"),
            6 => ("bam", "agave", "Agave + JitoBAM"),
            7 => ("sig", "sig", "Sig"),
            8 => ("rakurai", "agave", "Agave + Rakurai"),
            9 => ("harmonic", "firedancer", "Firedancer + Harmonic"),
            10 => ("harmonic", "agave", "Agave + Harmonic"),
            11 => ("harmonic", "frankendancer", "Frankendancer + Harmonic"),
            12 => ("bam", "frankendancer", "Frankendancer + JitoBAM"),
            13 => ("raiku", "agave", "Agave + Raiku"),
            _ => return None,
        })
    }
}

// A malformed gossip version is dropped so store never replaces the last known good version with it.
fn is_plausible_node_version(version: &str) -> bool {
    let numeric = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    let mut parts = version.splitn(3, '.');
    parts.next().is_some_and(numeric)
        && parts.next().is_some_and(numeric)
        && parts.next().is_some_and(|p| match p.split_once('-') {
            None => numeric(p),
            Some((patch, prerelease)) => {
                numeric(patch)
                    && !prerelease.is_empty()
                    && prerelease
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'.')
            }
        })
}

#[derive(Debug, Clone, Default)]
pub struct NodeContact {
    pub ip: Option<String>,
    pub gossip_port: Option<u16>,
    pub version: Option<String>,
    pub client_id: Option<u16>,
    pub client_name: Option<String>,
    pub client_vendor: Option<&'static str>,
    pub client_lineage: Option<&'static str>,
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

        let client_name = client_display_name(resolved, node.client_id.as_deref());

        out.insert(
            node.pubkey.clone(),
            NodeContact {
                ip,
                gossip_port,
                version,
                client_id: resolved.number(),
                client_name,
                client_vendor: resolved.vendor(),
                client_lineage: resolved.lineage(),
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
    if account.owner != solana_config_program::id() {
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
    let validator_info = rpc_client.get_program_accounts(&solana_config_program::id())?;

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

pub fn get_apy(
    rpc_client: &RpcClient,
    vote_accounts: &RpcVoteAccountStatus,
    credits: &HashMap<String, u64>,
) -> anyhow::Result<HashMap<String, f64>> {
    info!("Calculating APY");
    let inflation = rpc_client.get_inflation_rate()?.total;
    let inflation_taper = rpc_client.get_inflation_governor()?.taper;

    let epochs_in_year = 160; // @todo fix

    let activated_stake: HashMap<_, _> = vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
        .map(|v| (v.vote_pubkey.clone(), v.activated_stake))
        .collect();

    let commission: HashMap<_, _> = vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
        .map(|v| (v.vote_pubkey.clone(), v.commission))
        .collect();

    let total_activated_stake = activated_stake.values().sum::<u64>();

    let points: HashMap<_, _> = activated_stake
        .iter()
        .filter_map(|(node, stake)| {
            credits
                .get(node)
                .map(|credits| (node.clone(), *credits as u128 * *stake as u128))
        })
        .collect();

    let total_points = points.values().sum::<u128>();

    let mut total_rewards = 0.0;
    for epoch in 1..epochs_in_year + 1 {
        let tapered_inflation =
            inflation * (1.0 - inflation_taper).powf(epoch as f64 / epochs_in_year as f64);
        total_rewards += tapered_inflation / epochs_in_year as f64 * total_activated_stake as f64;
    }

    let mut apy = HashMap::new();
    for (node, points) in points.iter() {
        if let (Some(stake), Some(commission)) = (activated_stake.get(node), commission.get(node)) {
            let node_staker_rewards = (1.0 - *commission as f64 / 100.0) * *points as f64
                / total_points as f64
                * total_rewards;
            apy.insert(
                node.clone(),
                (*stake as f64 + node_staker_rewards) / *stake as f64 - 1.0,
            );
        }
    }

    Ok(apy)
}

// Relies on vote account layout and needs updating in case the authorized withdrawer position would change
pub fn get_withdraw_authorities(
    rpc_client: &RpcClient,
) -> anyhow::Result<HashSet<(String, String)>> {
    let mut withdraw_authorities: HashSet<(String, String)> = HashSet::default();
    let vote_program_id = solana_vote_program::id();
    let vote_accounts = rpc_client.get_program_accounts(&vote_program_id)?;

    for (account_pubkey, account) in vote_accounts {
        if account.data.len() < 68 {
            continue;
        }
        let authorized_withdrawer =
            Pubkey::new_from_array(account.data[36..68].try_into().map_err(|e| {
                anyhow::anyhow!(
                    "Failed to read vote account {account_pubkey} authorized_withdrawer: {e}"
                )
            })?);
        withdraw_authorities.insert((
            authorized_withdrawer.to_string(),
            account_pubkey.to_string(),
        ));
    }
    Ok(withdraw_authorities)
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

// Agave projects commissionBps onto the legacy percent this way; rounding down instead would let a
// validator above the 10% eligibility cap read as exactly at it.
fn bps_to_percent(bps: u16) -> u8 {
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
    for vote_addresses_chunk in vote_addresses.chunks(100) {
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
    if result.len() * 2 < queried {
        warn!(
            "Resolved commission for {} of {} validators: {} without a reward, {} served neither commission nor commissionBps",
            result.len(),
            queried,
            stats.no_reward,
            stats.unresolved
        );
    } else {
        info!(
            "Resolved commission for {} of {} validators: {} from commission, {} from commissionBps, {} without a reward, {} unresolved",
            result.len(),
            queried,
            stats.from_commission,
            stats.from_bps,
            stats.no_reward,
            stats.unresolved
        );
    }

    Ok(result)
}

pub fn get_self_stake(
    rpc_client: &RpcClient,
    epoch: Epoch,
    stake_history: &StakeHistory,
    bonds_url: &str,
    allow_zero_funded_bonds: bool,
    rpc_attempts: usize,
) -> anyhow::Result<HashMap<String, u64>> {
    let withdraw_authorities = get_withdraw_authorities(rpc_client)?;
    let mut self_stake = fetch_self_stake(
        rpc_client,
        withdraw_authorities,
        epoch,
        stake_history,
        rpc_attempts,
    )?;

    assert!(!self_stake.is_empty(), "Failed to fetch self stake data");

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
        *self_stake.entry(bond.vote_account).or_insert(0) += funded_amount_u64;
    }
    Ok(self_stake)
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
            rpc_client.get_program_accounts_with_config(
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

fn process_accounts_for_self_stake(
    accounts: Vec<(Pubkey, Account)>,
    self_stake: &mut HashMap<String, u64>,
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
                    activating: _,
                    deactivating: _,
                } = stake_account
                    .stake()
                    .unwrap()
                    .delegation
                    .stake_activating_and_deactivating(epoch, stake_history, None);
                if withdraw_authorities.contains(&(withdrawer_key, vote_key.clone()))
                    && effective != 0
                {
                    self_stake_assigned += 1;
                    update_self_stake(self_stake, &vote_key, effective);
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

fn update_self_stake(self_stake: &mut HashMap<String, u64>, vote_key: &str, lamports: u64) {
    let stake_entry = self_stake.entry(vote_key.to_string()).or_insert(0);
    *stake_entry += lamports;
}

pub fn fetch_self_stake(
    rpc_client: &RpcClient,
    withdraw_authorities: HashSet<(String, String)>,
    epoch: Epoch,
    stake_history: &StakeHistory,
    rpc_attemtps: usize,
) -> anyhow::Result<HashMap<String, u64>> {
    let mut self_stake: HashMap<String, u64> = HashMap::default();
    for page in 0..=u8::MAX {
        match fetch_stake_accounts_on_page(rpc_client, page, rpc_attemtps) {
            Ok(accounts) => {
                let processed = process_accounts_for_self_stake(
                    accounts,
                    &mut self_stake,
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

    Ok(self_stake)
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
    fn display_name_never_drops_a_client_the_node_reported() {
        let display = |raw| client_display_name(resolve_client_id(raw), raw);
        assert_eq!(display(Some("Unknown(8)")), Some("Rakurai".to_string()));
        assert_eq!(display(Some("AgaveBam")), Some("Agave Bam".to_string()));
        assert_eq!(
            display(Some("Unknown(86)")),
            Some("Unknown(86)".to_string())
        );
        assert_eq!(
            display(Some("brand-new/1.0")),
            Some("brand-new/1.0".to_string())
        );
        assert_eq!(display(Some("   ")), None);
        assert_eq!(display(None), None);
    }

    #[test]
    fn client_label_pairs_lineage_with_the_vendor_modification() {
        let label = |raw| resolve_client_id(Some(raw)).label();
        assert_eq!(label("Agave"), Some("Agave"));
        assert_eq!(label("Solana Labs"), Some("Agave"));
        assert_eq!(label("JitoLabs"), Some("Agave + Jito"));
        assert_eq!(label("AgaveBam"), Some("Agave + JitoBAM"));
        assert_eq!(label("AgavePaladin"), Some("Agave + Paladin"));
        assert_eq!(label("Unknown(8)"), Some("Agave + Rakurai"));
        assert_eq!(label("Unknown(10)"), Some("Agave + Harmonic"));
        assert_eq!(label("Raiku"), Some("Agave + Raiku"));
        assert_eq!(label("Frankendancer"), Some("Frankendancer"));
        assert_eq!(label("Unknown(11)"), Some("Frankendancer + Harmonic"));
        assert_eq!(label("Unknown(12)"), Some("Frankendancer + JitoBAM"));
        assert_eq!(label("Firedancer"), Some("Firedancer"));
        assert_eq!(label("Unknown(9)"), Some("Firedancer + Harmonic"));
        assert_eq!(label("Sig"), Some("Sig"));
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
