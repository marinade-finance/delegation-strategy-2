use crate::common::*;
use crate::slot_params::{
    baseline_slots_per_year, get_slot_params, nearest_slot_time_ms, SLOTS_IN_EPOCH,
};
use crate::solana_service::solana_client_with_timeout;
use crate::solana_service::*;
use anyhow::Context;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use serde_yaml;
use solana_client::{rpc_client::RpcClient, rpc_response::RpcVoteAccountStatus};
use solana_sdk::clock::Epoch;
use solana_sdk::epoch_info::EpochInfo;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct ValidatorsPerformanceParams {
    #[structopt(long = "with-rewards", help = "Whether to calculate APY and rewards.")]
    with_rewards: bool,

    #[structopt(long = "epoch", help = "Which epoch to use for epoch-based metrics.")]
    epoch: Option<Epoch>,

    #[structopt(
        long = "rpc-attempts",
        help = "How many times to retry the operation.",
        default_value = "10"
    )]
    rpc_attempts: usize,

    #[structopt(
        long = "rpc-timeout",
        help = "How long to wait for RPC response (seconds).",
        default_value = "300"
    )]
    rpc_timeout: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidatorRewards {
    pub commission_effective: Option<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClusterInflation {
    pub sol_total_supply: u64,
    pub inflation: f64,
    pub inflation_taper: f64,
}

// Snapshots predating the numeric client_id hold the rendered string, and store reads them back post-deploy.
fn deserialize_client_id<'de, D>(deserializer: D) -> Result<Option<u16>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumberOrRendered {
        Number(u16),
        Rendered(String),
    }

    Ok(
        match Option::<NumberOrRendered>::deserialize(deserializer)? {
            Some(NumberOrRendered::Number(id)) => Some(id),
            Some(NumberOrRendered::Rendered(raw)) => resolve_client_id(Some(&raw)).number(),
            None => None,
        },
    )
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ValidatorPerformance {
    pub commission: u8,
    pub version: Option<String>,
    #[serde(default, deserialize_with = "deserialize_client_id")]
    pub client_id: Option<u16>,
    #[serde(default)]
    pub client_id_raw: Option<String>,
    #[serde(default)]
    pub feature_set: Option<u32>,
    #[serde(default)]
    pub shred_version: Option<u16>,
    pub credits: u64,
    pub leader_slots: usize,
    pub blocks_produced: usize,
    pub skip_rate: f64,
    pub delinquent: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ValidatorsPerformanceSnapshot {
    pub epoch: Epoch,
    pub epoch_slot: u64,
    pub transaction_count: u64,
    pub created_at: String,
    // Snapshots predating the gate read describe epochs that provably ran at the baseline slot time.
    #[serde(default = "baseline_slots_per_year")]
    pub slots_per_year: f64,
    pub cluster_inflation: Option<ClusterInflation>,
    pub validators: HashMap<String, ValidatorPerformance>,
    pub rewards: Option<HashMap<String, ValidatorRewards>>,
}

/// A mistyped gate pubkey reads exactly like "not activated", and only the clock can tell them apart.
fn warn_on_slot_time_divergence(
    client: &RpcClient,
    current_epoch_info: &EpochInfo,
) -> anyhow::Result<()> {
    let Some(measured_ms) = measure_milliseconds_per_slot(client, current_epoch_info)? else {
        return Ok(());
    };
    let nominal_ms = get_slot_params(client, current_epoch_info.epoch)?.slot_time_ms;
    let nearest_ms = nearest_slot_time_ms(measured_ms);
    if nearest_ms != nominal_ms {
        warn!(
            "Epoch {} runs at a measured {measured_ms}ms/slot, nearest the {nearest_ms}ms row, but {nominal_ms}ms was selected. Check the slot time gate pubkeys.",
            current_epoch_info.epoch
        );
    }
    Ok(())
}

/// RPC answers for the current epoch; a backfilled one was minted at an earlier point on agave's taper curve.
fn cluster_inflation_at_epoch(
    client: &RpcClient,
    epoch: Epoch,
    slots_per_year: f64,
) -> anyhow::Result<ClusterInflation> {
    let rate = client.get_inflation_rate()?;
    let governor = client.get_inflation_governor()?;
    let epochs_behind = rate.epoch.checked_sub(epoch).with_context(|| {
        format!(
            "Epoch {epoch} is ahead of the cluster's current epoch {}",
            rate.epoch
        )
    })?;

    let (inflation, sol_total_supply) = inflation_and_supply_at(
        rate.total,
        client.supply()?.value.total,
        governor.taper,
        governor.initial,
        governor.terminal,
        slots_per_year / SLOTS_IN_EPOCH as f64,
        epochs_behind,
    );

    Ok(ClusterInflation {
        sol_total_supply,
        inflation,
        inflation_taper: governor.taper,
    })
}

fn inflation_and_supply_at(
    inflation_now: f64,
    supply_now: u64,
    taper: f64,
    initial: f64,
    terminal: f64,
    nominal_epochs_per_year: f64,
    epochs_behind: u64,
) -> (f64, u64) {
    if epochs_behind == 0 {
        return (inflation_now, supply_now);
    }
    // Agave's curve is `max(initial * (1 - taper)^year, terminal)`: walking it back is bounded by `initial`, pinned by `terminal`.
    let step_back = if inflation_now > terminal {
        (1.0 - taper).powf(-1.0 / nominal_epochs_per_year)
    } else {
        1.0
    };

    let mut inflation = inflation_now;
    let mut supply = supply_now as f64;
    for _ in 0..epochs_behind {
        // Un-mint at the rate that epoch ran, not today's: a flat rate understates minting as the walk deepens.
        supply /= 1.0 + inflation / nominal_epochs_per_year;
        inflation = (inflation * step_back).min(initial);
    }

    (inflation, supply as u64)
}

pub fn validators_performance(
    client: &RpcClient,
    epoch: Epoch,
    vote_accounts: &RpcVoteAccountStatus,
    rpc_attempts: usize,
    node_info: &HashMap<String, NodeContact>,
) -> anyhow::Result<HashMap<String, ValidatorPerformance>> {
    let mut validators: HashMap<String, ValidatorPerformance> = Default::default();

    let delinquent: HashSet<_> = vote_accounts
        .delinquent
        .iter()
        .map(|v| v.vote_pubkey.clone())
        .collect();
    // block production is the first RPC after the whois fetch; retry so a keep-alive socket dropped during that idle gap doesn't abort the whole snapshot
    let production_by_validator = retry_blocking(
        || get_block_production_by_validator(client, epoch),
        QuadraticBackoffStrategy::iter_durations(rpc_attempts),
        |err, attempt, backoff| {
            warn!("Attempt {attempt} to get block production failed: {err:?}, retrying in {backoff:?}")
        },
    )?;
    let credits = get_credits(client, epoch)?;

    for vote_account in vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
    {
        let vote_pubkey = vote_account.vote_pubkey.clone();
        let identity = vote_account.node_pubkey.clone();
        let (leader_slots, blocks_produced) = production_by_validator
            .get(&identity)
            .cloned()
            .unwrap_or((0, 0));

        let node = node_info.get(&identity);

        validators.insert(
            vote_pubkey.clone(),
            ValidatorPerformance {
                commission: vote_account.commission,
                version: node.and_then(|n| n.version.clone()),
                client_id: node.and_then(|n| n.client_id),
                client_id_raw: node.and_then(|n| n.client_id_raw.clone()),
                feature_set: node.and_then(|n| n.feature_set),
                shred_version: node.and_then(|n| n.shred_version),
                credits: credits.get(&vote_pubkey).cloned().unwrap_or(0),
                leader_slots,
                blocks_produced,
                skip_rate: if leader_slots == 0 {
                    0f64
                } else {
                    1f64 - (blocks_produced as f64 / leader_slots as f64)
                },
                delinquent: delinquent.contains(&vote_pubkey),
            },
        );
    }

    Ok(validators)
}

pub fn validator_rewards(
    client: &RpcClient,
    epoch: Epoch,
    vote_accounts: &RpcVoteAccountStatus,
) -> anyhow::Result<HashMap<String, ValidatorRewards>> {
    let commission_from_rewards =
        get_commission_from_inflation_rewards(client, vote_accounts, Some(epoch))?;

    Ok(vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
        .map(|vote_account| {
            (
                vote_account.vote_pubkey.clone(),
                ValidatorRewards {
                    commission_effective: commission_from_rewards
                        .get(&vote_account.vote_pubkey)
                        .cloned(),
                },
            )
        })
        .collect())
}

pub fn collect_validators_performance_info(
    common_params: CommonParams,
    performance_params: ValidatorsPerformanceParams,
) -> anyhow::Result<()> {
    info!("Collecting snaphost of validators' performance");
    let client = solana_client_with_timeout(
        common_params.rpc_url,
        Duration::from_secs(performance_params.rpc_timeout),
        common_params.commitment,
    );

    let created_at = chrono::Utc::now();
    let current_epoch_info = client.get_epoch_info()?;
    let epoch = performance_params.epoch.unwrap_or(current_epoch_info.epoch);
    info!("Current epoch: {current_epoch_info:?}");
    info!("Looking at epoch: {epoch}");

    let vote_accounts = client.get_vote_accounts()?;
    info!(
        "Total vote accounts found: {}",
        vote_accounts.current.len() + vote_accounts.delinquent.len()
    );
    info!(
        "Delinquent vote accounts found: {}",
        vote_accounts.delinquent.len()
    );

    let node_info = get_cluster_nodes_info(&client)?;
    let validators = validators_performance(
        &client,
        epoch,
        &vote_accounts,
        performance_params.rpc_attempts,
        &node_info,
    )?;

    let rewards = if performance_params.with_rewards {
        Some(validator_rewards(&client, epoch, &vote_accounts)?)
    } else {
        None
    };

    let slots_per_year = get_slot_params(&client, epoch)?.slots_per_year;

    // Every run persists slots_per_year, so the only detector of a mistyped gate must not sit behind --with-rewards.
    if let Err(err) = warn_on_slot_time_divergence(&client, &current_epoch_info) {
        warn!("Could not cross-check the nominal slot time against the clock: {err}");
    }

    let cluster_inflation = if performance_params.with_rewards {
        Some(cluster_inflation_at_epoch(&client, epoch, slots_per_year)?)
    } else {
        None
    };

    serde_yaml::to_writer(
        std::io::stdout(),
        &ValidatorsPerformanceSnapshot {
            epoch,
            epoch_slot: current_epoch_info.slot_index,
            transaction_count: current_epoch_info.transaction_count.unwrap(),
            created_at: created_at.to_string(),
            slots_per_year,
            cluster_inflation,
            validators,
            rewards,
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAPER: f64 = 0.15;
    const INITIAL: f64 = 0.08;
    const TERMINAL: f64 = 0.015;
    const NOMINAL_EPOCHS_PER_YEAR: f64 = 182.6211;
    const SUPPLY: u64 = 600_000_000_000_000_000;
    const INFLATION: f64 = 0.043;

    fn at_epochs_behind(inflation_now: f64, epochs_behind: u64) -> (f64, u64) {
        inflation_and_supply_at(
            inflation_now,
            SUPPLY,
            TAPER,
            INITIAL,
            TERMINAL,
            NOMINAL_EPOCHS_PER_YEAR,
            epochs_behind,
        )
    }

    #[test]
    fn the_current_epoch_needs_no_correction() {
        let (inflation, supply) = at_epochs_behind(INFLATION, 0);
        assert_eq!(inflation, INFLATION);
        assert_eq!(supply, SUPPLY);
    }

    #[test]
    fn a_backfilled_epoch_was_minted_at_a_higher_rate_on_a_smaller_supply() {
        let (inflation, supply) = at_epochs_behind(INFLATION, 1);
        let expected_inflation = INFLATION * (1.0 - TAPER).powf(-1.0 / NOMINAL_EPOCHS_PER_YEAR);
        assert!((inflation - expected_inflation).abs() / expected_inflation < 1e-12);
        assert!(inflation > INFLATION);

        let expected_supply = SUPPLY as f64 / (1.0 + INFLATION / NOMINAL_EPOCHS_PER_YEAR);
        assert!((supply as f64 - expected_supply).abs() / expected_supply < 1e-12);
        assert!(supply < SUPPLY);
    }

    #[test]
    fn a_rate_already_at_the_terminal_does_not_move() {
        let (inflation, supply) = at_epochs_behind(TERMINAL, 3);
        assert_eq!(inflation, TERMINAL);
        assert!(supply < SUPPLY);
    }

    #[test]
    fn a_deep_backfill_cannot_exceed_the_rate_the_curve_started_at() {
        // ~11 years back, where the unbounded curve would read 25% - a rate the cluster never had.
        let (inflation, _) = at_epochs_behind(INFLATION, 2_000);
        assert_eq!(inflation, INITIAL);
    }

    #[test]
    fn a_deep_backfill_un_mints_at_the_rates_those_epochs_ran() {
        let (_, supply) = at_epochs_behind(INFLATION, 2_000);
        let at_todays_rate =
            SUPPLY as f64 / (1.0 + INFLATION / NOMINAL_EPOCHS_PER_YEAR).powi(2_000);
        // Those epochs averaged ~6.5% against today's 4.3%, so a flat walk leaves ~25% too much supply.
        assert!(
            (supply as f64) < at_todays_rate * 0.8,
            "{supply} is not materially below the flat-rate {at_todays_rate}"
        );
    }
}
