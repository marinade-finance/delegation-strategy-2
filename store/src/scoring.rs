use crate::dto::{BlacklistRecord, ScoringRunRecord, UnstakeHint, UnstakeHintRecord};
use rust_decimal::prelude::*;
use std::collections::{HashMap, HashSet};
use tokio_postgres::Client;

fn load_blacklist(blacklist_path: &String) -> anyhow::Result<HashMap<String, HashSet<String>>> {
    let mut blacklist: Vec<BlacklistRecord> = Default::default();
    let mut rdr = csv::Reader::from_path(blacklist_path)?;
    for result in rdr.deserialize() {
        blacklist.push(result?);
    }

    Ok(blacklist.into_iter().fold(
        HashMap::new(),
        |mut acc, BlacklistRecord { vote_account, code }| {
            acc.entry(vote_account).or_default().insert(code);

            acc
        },
    ))
}

async fn voter_max_commission_in_epoch(
    psql_client: &Client,
    epoch: u64,
) -> anyhow::Result<HashMap<String, u8>> {
    log::info!("Loading max commission per voter in epoch: {}", epoch);
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
    log::info!(
        "Loading list of validators with Marinade stake in epoch: {}",
        epoch
    );
    Ok(psql_client
        .query(
            "SELECT
                    vote_account,
                    (marinade_stake / 1e9)::double precision as marinade_stake
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
    blacklist_path: &String,
    epoch: u64,
) -> anyhow::Result<Vec<UnstakeHintRecord>> {
    log::info!("Loading unstake hints in epoch: {}", epoch);
    let max_allowed_commission = 10;
    let mut hints: HashMap<_, HashSet<_>> = Default::default();

    let marinade_staked_validators =
        voters_with_marinade_stake_in_epoch(psql_client, epoch).await?;
    let commissions_in_this_epoch = voter_max_commission_in_epoch(psql_client, epoch).await?;
    let commissions_in_previous_epoch = if epoch > 0 {
        voter_max_commission_in_epoch(psql_client, epoch - 1).await?
    } else {
        Default::default()
    };
    let blacklist = load_blacklist(blacklist_path)?;

    for (vote_account, commission) in commissions_in_this_epoch {
        if commission > max_allowed_commission {
            hints
                .entry(vote_account)
                .or_default()
                .insert(UnstakeHint::HighCommission);
        }
    }

    for (vote_account, commission) in commissions_in_previous_epoch {
        if commission > max_allowed_commission {
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

    Ok(marinade_staked_validators
        .into_iter()
        .filter_map(
            |(vote_account, marinade_stake)| match hints.get(&vote_account).cloned() {
                Some(hints) => Some(UnstakeHintRecord {
                    vote_account,
                    marinade_stake,
                    hints,
                }),
                _ => None,
            },
        )
        .collect())
}

pub async fn load_scoring_runs(psql_client: &Client) -> anyhow::Result<Vec<ScoringRunRecord>> {
    log::info!("Querying all scoring runs...");
    Ok(psql_client
        .query(
            "
            SELECT
                scoring_run_id::numeric,
                created_at,
                epoch,
                components,
                component_weights,
                ui_id
            FROM scoring_runs
            ORDER BY scoring_run_id DESC",
            &[],
        )
        .await?
        .into_iter()
        .map(|scoring_run| ScoringRunRecord {
            scoring_run_id: scoring_run.get("scoring_run_id"),
            created_at: scoring_run.get("created_at"),
            epoch: scoring_run.get("epoch"),
            components: scoring_run.get("components"),
            component_weights: scoring_run.get("component_weights"),
            ui_id: scoring_run.get("ui_id"),
        })
        .collect())
}
