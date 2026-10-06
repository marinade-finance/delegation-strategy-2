use crate::dto::{
    BlacklistRecord, GlobalUnstakeHintRecord, ScoringRunRecord, UnstakeHint, UnstakeHintRecord,
    ValidatorScoreRecord,
};
use crate::warehouse::Warehouse;
use csv::{required, Column};
use rust_decimal::prelude::*;
use std::collections::{HashMap, HashSet};

const MAX_ALLOWED_COMMISSION: u8 = 10;
const MIN_REQUIRED_CREDITS_PERFORMANCE: f64 = 0.5;

const BLACKLIST_COLUMNS: [Column; 2] = [required("vote_account"), required("code")];

fn load_blacklist(blacklist_path: &String) -> anyhow::Result<HashMap<String, HashSet<String>>> {
    let blacklist: Vec<BlacklistRecord> =
        csv::load_path(std::path::Path::new(blacklist_path), &BLACKLIST_COLUMNS)?;

    Ok(blacklist.into_iter().fold(
        HashMap::new(),
        |mut acc, BlacklistRecord { vote_account, code }| {
            acc.entry(vote_account).or_default().insert(code);

            acc
        },
    ))
}

/// The highest commission a validator was seen at in one epoch, over every
/// observation of it: the changes, and the snapshot's own three columns.
fn voter_max_commission_in_epoch(
    warehouse: &Warehouse,
    epoch: u64,
) -> anyhow::Result<HashMap<String, u8>> {
    log::info!("Loading max commission per voter in epoch: {epoch}");
    let mut commissions: HashMap<String, u8> = Default::default();

    let Some(snapshot) = warehouse.snapshots.get(&epoch) else {
        return Ok(commissions);
    };
    let changes = warehouse.commissions_of(epoch);

    for (vote_account, validator) in snapshot.iter() {
        let Some(observed) = changes.get(vote_account) else {
            continue;
        };
        let highest = observed
            .iter()
            .map(|change| change.commission)
            .chain([
                validator.commission_effective.unwrap_or_default(),
                validator.commission_max_observed.unwrap_or_default(),
                validator.commission_advertised.unwrap_or_default(),
            ])
            .max()
            .unwrap_or_default();
        commissions.insert(vote_account.clone(), highest.try_into()?);
    }

    Ok(commissions)
}

fn voters_with_marinade_stake_in_epoch(warehouse: &Warehouse, epoch: u64) -> HashMap<String, f64> {
    log::info!("Loading list of validators with Marinade stake in epoch: {epoch}");
    let Some(snapshot) = warehouse.snapshots.get(&epoch) else {
        return Default::default();
    };

    snapshot
        .iter()
        .filter(|(_, validator)| validator.marinade_stake > Decimal::ZERO)
        .map(|(vote_account, validator)| (vote_account.clone(), to_sol(validator.marinade_stake)))
        .collect()
}

fn voters_credits_performance_in_epoch(warehouse: &Warehouse, epoch: u64) -> HashMap<String, f64> {
    log::info!("Loading list of poor voters: {epoch}");
    let Some(snapshot) = warehouse.snapshots.get(&epoch) else {
        return Default::default();
    };

    let total_stake: Decimal = snapshot
        .values()
        .map(|validator| validator.activated_stake)
        .sum();
    let weighted_credits: Decimal = snapshot
        .values()
        .map(|validator| validator.activated_stake * validator.credits)
        .sum();
    let stake_weighted_avg_credits = match total_stake.is_zero() {
        true => 0f64,
        false => (weighted_credits / total_stake)
            .to_f64()
            .unwrap_or_default(),
    };

    snapshot
        .iter()
        .map(|(vote_account, validator)| {
            let performance = match stake_weighted_avg_credits {
                0f64 => 0f64,
                average => validator.credits.to_f64().unwrap_or_default() / average,
            };
            (vote_account.clone(), performance)
        })
        .collect()
}

fn to_sol(lamports: Decimal) -> f64 {
    (lamports / Decimal::from(1_000_000_000u64))
        .to_f64()
        .unwrap_or_default()
}

pub fn load_unstake_hints(
    warehouse: &Warehouse,
    blacklist_path: &String,
    epoch: u64,
) -> anyhow::Result<HashMap<String, HashSet<UnstakeHint>>> {
    log::info!("Loading unstake hints in epoch: {epoch}");
    let mut hints: HashMap<_, HashSet<_>> = Default::default();

    let commissions_in_this_epoch = voter_max_commission_in_epoch(warehouse, epoch)?;
    let commissions_in_previous_epoch = match epoch > 0 {
        true => voter_max_commission_in_epoch(warehouse, epoch - 1)?,
        false => Default::default(),
    };
    let voters_credits_performance = voters_credits_performance_in_epoch(warehouse, epoch);
    let blacklist = load_blacklist(blacklist_path)?;

    for (vote_account, commission) in commissions_in_this_epoch {
        if commission > MAX_ALLOWED_COMMISSION {
            hints
                .entry(vote_account)
                .or_default()
                .insert(UnstakeHint::HighCommission);
        }
    }

    for (vote_account, commission) in commissions_in_previous_epoch {
        if commission > MAX_ALLOWED_COMMISSION {
            hints
                .entry(vote_account)
                .or_default()
                .insert(UnstakeHint::HighCommissionInPreviousEpoch);
        }
    }

    for (vote_account, _) in blacklist {
        hints
            .entry(vote_account)
            .or_default()
            .insert(UnstakeHint::Blacklist);
    }

    for (vote_account, performance) in voters_credits_performance {
        if performance < MIN_REQUIRED_CREDITS_PERFORMANCE {
            hints
                .entry(vote_account)
                .or_default()
                .insert(UnstakeHint::LowCredits);
        }
    }

    Ok(hints)
}

pub fn load_marinade_unstake_hint_records(
    warehouse: &Warehouse,
    blacklist_path: &String,
    epoch: u64,
) -> anyhow::Result<Vec<UnstakeHintRecord>> {
    log::info!("Loading Marinade unstake hint records in epoch: {epoch}");

    let hints = load_unstake_hints(warehouse, blacklist_path, epoch)?;
    let marinade_staked_validators = voters_with_marinade_stake_in_epoch(warehouse, epoch);

    Ok(marinade_staked_validators
        .into_iter()
        .filter_map(|(vote_account, marinade_stake)| {
            hints
                .get(&vote_account)
                .cloned()
                .map(|hints| UnstakeHintRecord {
                    vote_account,
                    marinade_stake,
                    hints: hints.into_iter().collect(),
                })
        })
        .collect())
}

pub fn load_global_unstake_hint_records(
    warehouse: &Warehouse,
    blacklist_path: &String,
    epoch: u64,
) -> anyhow::Result<Vec<GlobalUnstakeHintRecord>> {
    log::info!("Loading global unstake hint records in epoch: {epoch}");

    let hints = load_unstake_hints(warehouse, blacklist_path, epoch)?;

    Ok(hints
        .into_iter()
        .map(|(vote_account, hints)| GlobalUnstakeHintRecord {
            vote_account,
            hints: hints.into_iter().collect(),
        })
        .collect())
}

pub fn load_all_scores(warehouse: &Warehouse) -> HashMap<Decimal, Vec<ValidatorScoreRecord>> {
    warehouse
        .scoring
        .values()
        .map(|breakdowns| {
            let mut scores = breakdowns.scores.clone();
            scores.sort_by_key(|score| score.rank);
            (Decimal::from(breakdowns.scoring_run_id), scores)
        })
        .collect()
}

pub fn load_scoring_runs(warehouse: &Warehouse) -> Vec<ScoringRunRecord> {
    warehouse
        .scoring
        .values()
        .rev()
        .map(|breakdowns| breakdowns.scoring_run())
        .collect()
}
