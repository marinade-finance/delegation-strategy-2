use crate::dto::{
    BlacklistRecord, ScoringRunRecord, UnstakeHint, UnstakeHintRecord, ValidatorScoreRecord,
};
use chrono::{DateTime, Utc};
use rust_decimal::prelude::*;
use std::collections::{HashMap, HashSet};
use tokio_postgres::Client;

const MAX_ALLOWED_COMMISSION: u8 = 10;
const MIN_REQUIRED_CREDITS_PERFORMANCE: f64 = 0.5;

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

async fn voters_credits_performance_in_epoch(
    psql_client: &Client,
    epoch: u64,
) -> anyhow::Result<HashMap<String, f64>> {
    log::info!("Loading list of poor voters: {}", epoch);
    Ok(psql_client
        .query(
            "WITH stats AS (SELECT AVG(activated_stake * credits) / avg(activated_stake) as stake_weighted_avg_credits FROM validators WHERE epoch = $1)
            SELECT
                vote_account,
                credits,
                coalesce(credits / stake_weighted_avg_credits, 0)::double precision as credits_performance
            FROM validators left join stats on 1 = 1
            WHERE epoch = $1",
            &[&Decimal::from(epoch)],
        )
        .await?
        .iter()
        .map(|row| (row.get("vote_account"), row.get("credits_performance")))
        .collect())
}

pub async fn load_unstake_hints(
    psql_client: &Client,
    blacklist_path: &String,
    epoch: u64,
) -> anyhow::Result<Vec<UnstakeHintRecord>> {
    log::info!("Loading unstake hints in epoch: {}", epoch);
    let mut hints: HashMap<_, HashSet<_>> = Default::default();

    let marinade_staked_validators =
        voters_with_marinade_stake_in_epoch(psql_client, epoch).await?;
    let commissions_in_this_epoch = voter_max_commission_in_epoch(psql_client, epoch).await?;
    let commissions_in_previous_epoch = if epoch > 0 {
        voter_max_commission_in_epoch(psql_client, epoch - 1).await?
    } else {
        Default::default()
    };
    let voters_credits_performance =
        voters_credits_performance_in_epoch(psql_client, epoch).await?;
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

pub async fn load_all_scores(
    psql_client: &Client,
) -> anyhow::Result<HashMap<String, Vec<ValidatorScoreRecord>>> {
    log::info!("Querying all scores...");
    let rows = psql_client
        .query(
            "
            SELECT vote_account,
                score,
                rank,
                ui_hints,
                component_scores,
                component_ranks,
                component_values,
                eligible_stake_algo,
                eligible_stake_mnde,
                eligible_stake_msol,
                target_stake_algo,
                target_stake_mnde,
                target_stake_msol,
                scores.scoring_run_id,
                scoring_runs.created_at as created_at
            FROM scores
            LEFT JOIN scoring_runs ON scoring_runs.scoring_run_id = scores.scoring_run_id
            ORDER BY rank",
            &[],
        )
        .await?;

    let records: HashMap<_, Vec<_>> = {
        log::info!("Aggregating scores records...");
        let mut records: HashMap<_, Vec<_>> = Default::default();
        for row in rows {
            let vote_account: String = row.get("vote_account");
            let scores = records
                .entry(vote_account.clone())
                .or_insert(Default::default());
            scores.push(ValidatorScoreRecord {
                vote_account: vote_account.clone(),
                score: row.get("score"),
                rank: row.get("rank"),
                ui_hints: row.get("ui_hints"),
                component_scores: row.get("component_scores"),
                component_ranks: row.get("component_ranks"),
                component_values: row.get("component_values"),
                eligible_stake_algo: row.get("eligible_stake_algo"),
                eligible_stake_mnde: row.get("eligible_stake_mnde"),
                eligible_stake_msol: row.get("eligible_stake_msol"),
                target_stake_algo: row
                    .get::<_, Decimal>("target_stake_algo")
                    .try_into()
                    .unwrap(),
                target_stake_mnde: row
                    .get::<_, Decimal>("target_stake_mnde")
                    .try_into()
                    .unwrap(),
                target_stake_msol: row
                    .get::<_, Decimal>("target_stake_msol")
                    .try_into()
                    .unwrap(),
                scoring_run_id: row.get("scoring_run_id"),
                created_at: row
                    .get::<_, DateTime<Utc>>("created_at")
                    .try_into()
                    .unwrap(),
            })
        }

        records
    };
    log::info!("Records prepared...");
    Ok(records)
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
