use crate::blacklist::db::load_active_accounts;
use crate::dto::{GlobalUnstakeHintRecord, UnstakeHint, UnstakeHintRecord};
use crate::vote_performance::load_vote_performance;
use rust_decimal::prelude::*;
use std::collections::{HashMap, HashSet};
use tokio_postgres::Client;

const MAX_ALLOWED_COMMISSION: u8 = 10;
const MIN_REQUIRED_CREDITS_PERFORMANCE: f64 = 0.5;

async fn voter_max_commission_in_epoch(
    psql_client: &Client,
    epoch: u64,
) -> anyhow::Result<HashMap<String, u8>> {
    log::info!("Loading max commission per voter in epoch: {epoch}");
    let mut commissions: HashMap<_, _> = Default::default();

    let rows = psql_client
        .query(
            "SELECT
                    validators.vote_account,
                    MAX(GREATEST(
                        commission,
                        COALESCE(commission_effective, 0),
                        COALESCE(commission_max_observed, 0),
                        COALESCE(commission_advertised, 0)
                    )) commission
                FROM validators LEFT JOIN commissions on validators.vote_account = commissions.vote_account
                WHERE commissions.epoch = $1 and validators.epoch = $1
                GROUP BY validators.vote_account",
            &[&Decimal::from(epoch)],
        )
        .await?;

    for row in rows {
        commissions.insert(
            row.get("vote_account"),
            row.get::<_, i32>("commission").try_into()?,
        );
    }

    Ok(commissions)
}

async fn voters_with_marinade_stake_in_epoch(
    psql_client: &Client,
    epoch: u64,
) -> anyhow::Result<HashMap<String, f64>> {
    log::info!("Loading list of validators with Marinade stake in epoch: {epoch}");
    Ok(psql_client
        .query(
            "SELECT
                    vote_account,
                    (marinade_stake / 1e9)::double precision AS marinade_stake
                FROM validators
                WHERE marinade_stake > 0 AND epoch = $1",
            &[&Decimal::from(epoch)],
        )
        .await?
        .iter()
        .map(|row| (row.get("vote_account"), row.get("marinade_stake")))
        .collect())
}

pub async fn load_unstake_hints(
    psql_client: &Client,
    epoch: u64,
) -> anyhow::Result<HashMap<String, HashSet<UnstakeHint>>> {
    log::info!("Loading unstake hints in epoch: {epoch}");
    let mut hints: HashMap<_, HashSet<_>> = Default::default();

    let commissions_in_this_epoch = voter_max_commission_in_epoch(psql_client, epoch).await?;
    let commissions_in_previous_epoch = if epoch > 0 {
        voter_max_commission_in_epoch(psql_client, epoch - 1).await?
    } else {
        Default::default()
    };
    let vote_performance = load_vote_performance(psql_client, epoch, epoch).await?;
    let blacklist = load_active_accounts(psql_client).await?;

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

    for vote_account in blacklist {
        hints
            .entry(vote_account)
            .or_default()
            .insert(UnstakeHint::Blacklist);
    }

    for ((_, vote_account), performance) in vote_performance {
        if performance < MIN_REQUIRED_CREDITS_PERFORMANCE {
            hints
                .entry(vote_account)
                .or_default()
                .insert(UnstakeHint::LowCredits);
        }
    }

    Ok(hints)
}

pub async fn load_marinade_unstake_hint_records(
    psql_client: &Client,
    epoch: u64,
) -> anyhow::Result<Vec<UnstakeHintRecord>> {
    log::info!("Loading Marinade unstake hint records in epoch: {epoch}");

    let hints = load_unstake_hints(psql_client, epoch).await?;

    let marinade_staked_validators =
        voters_with_marinade_stake_in_epoch(psql_client, epoch).await?;

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

pub async fn load_global_unstake_hint_records(
    psql_client: &Client,
    epoch: u64,
) -> anyhow::Result<Vec<GlobalUnstakeHintRecord>> {
    log::info!("Loading global unstake hint records in epoch: {epoch}");

    let hints = load_unstake_hints(psql_client, epoch).await?;

    Ok(hints
        .into_iter()
        .map(|(vote_account, hints)| GlobalUnstakeHintRecord {
            vote_account,
            hints: hints.into_iter().collect(),
        })
        .collect())
}
