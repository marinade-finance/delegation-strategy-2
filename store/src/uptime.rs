use crate::utils::*;
use chrono::{DateTime, Duration, Utc};
use clap::Parser;
use collect::validators_performance::{ValidatorPerformance, ValidatorsPerformanceSnapshot};
use log::{debug, info, warn};
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::{HashMap, HashSet};
use tokio_postgres::types::ToSql;
use tokio_postgres::Client;

#[derive(Debug, Parser)]
pub struct StoreUptimeParams {
    #[arg(long = "snapshot-file")]
    snapshot_path: String,
}

static UP: &str = "UP";
static DOWN: &str = "DOWN";

// lastVote is 0 when the vote state has no votes, which SIMD-0357 makes the case under Alpenglow.
// Alpenglow credits of a live validator grow every slot.
fn is_down(performance: &ValidatorPerformance, last_credits: Option<Decimal>) -> bool {
    if performance.last_vote.is_some_and(|last_vote| last_vote > 0) {
        return performance.delinquent;
    }
    match (performance.credits_total, last_credits) {
        (Some(credits_total), Some(last_credits)) => Decimal::from(credits_total) <= last_credits,
        _ => performance.delinquent,
    }
}

fn status_from_down(down: bool) -> &'static str {
    if down {
        DOWN
    } else {
        UP
    }
}

pub async fn store_uptime(
    params: StoreUptimeParams,
    psql_client: &mut Client,
) -> anyhow::Result<()> {
    info!("Storing uptime...");

    let snapshot_file = std::fs::File::open(params.snapshot_path)?;
    let snapshot: ValidatorsPerformanceSnapshot = serde_yaml::from_reader(snapshot_file)?;
    let mut validators_with_extended_status: HashSet<String> = HashSet::new();
    let snapshot_epoch: Decimal = snapshot.epoch.into();
    let snapshot_created_at: DateTime<Utc> = snapshot.created_at.parse().unwrap();
    let default_status_end_at = snapshot_created_at
        .checked_add_signed(Duration::minutes(1))
        .unwrap();
    let status_max_delay_to_extend = Duration::minutes(5);
    let mut records_extensions: HashMap<i64, (DateTime<Utc>, Option<Decimal>)> = Default::default();

    info!("Loaded the snapshot");

    let latest_rows = psql_client
        .query(
            "
        SELECT DISTINCT ON (vote_account)
            id,
            vote_account,
            status,
            epoch,
            start_at,
            end_at,
            last_credits
        FROM uptimes
        ORDER BY vote_account, end_at DESC
    ",
            &[],
        )
        .await?;
    let last_credits: HashMap<&str, Decimal> = latest_rows
        .iter()
        .filter_map(|row| {
            row.get::<_, Option<Decimal>>("last_credits")
                .map(|credits| (row.get("vote_account"), credits))
        })
        .collect();
    let downs: HashMap<&str, bool> = snapshot
        .validators
        .iter()
        .map(|(vote_account, performance)| {
            (
                vote_account.as_str(),
                is_down(
                    performance,
                    last_credits.get(vote_account.as_str()).copied(),
                ),
            )
        })
        .collect();
    let credits_totals: HashMap<&str, Option<Decimal>> = snapshot
        .validators
        .iter()
        .map(|(vote_account, performance)| {
            (
                vote_account.as_str(),
                performance.credits_total.map(Decimal::from),
            )
        })
        .collect();

    for row in latest_rows.iter() {
        let id: i64 = row.get("id");
        let vote_account: &str = row.get("vote_account");
        let status: &str = row.get("status");
        let epoch: Decimal = row.get("epoch");
        let start_at: DateTime<Utc> = row.get("start_at");
        let end_at: DateTime<Utc> = row.get("end_at");
        let latest_end_extension_at = end_at
            .checked_add_signed(status_max_delay_to_extend)
            .unwrap();

        if let Some(down) = downs.get(vote_account) {
            let status_from_snapshot = status_from_down(*down);
            let credits_total = credits_totals[vote_account];
            if latest_end_extension_at > snapshot_created_at {
                if status == status_from_snapshot && epoch == snapshot_epoch {
                    validators_with_extended_status.insert(vote_account.to_string());
                    records_extensions.insert(id, (default_status_end_at, credits_total));
                } else {
                    records_extensions.insert(id, (snapshot_created_at, credits_total));
                }
            }
        }

        debug!("found uptime record: {id} {vote_account} {status} {start_at} {end_at}");
    }

    let mut query = UpdateQueryCombiner::new(
        "uptimes".to_string(),
        "end_at = u.end_at, last_credits = COALESCE(u.last_credits, uptimes.last_credits)"
            .to_string(),
        "u(id, end_at, last_credits)".to_string(),
        "uptimes.id = u.id".to_string(),
    );

    for (id, (status_end_at, credits_total)) in records_extensions.iter() {
        let mut params: Vec<&(dyn ToSql + Sync)> = vec![id, status_end_at, credits_total];
        query.add(
            &mut params,
            HashMap::from_iter([
                (0, "BIGINT".into()),
                (1, "TIMESTAMP WITH TIME ZONE".into()),
                (2, "NUMERIC".into()),
            ]),
        );
    }
    query.execute(psql_client).await?;
    info!("Extended previous {} uptimes", records_extensions.len());

    let mut query = InsertQueryCombiner::new(
        "uptimes".to_string(),
        "vote_account, status, epoch, start_at, end_at, last_credits".to_string(),
    );

    for vote_account in snapshot.validators.keys() {
        if !validators_with_extended_status.contains(vote_account) {
            let credits_total = &credits_totals[vote_account.as_str()];
            if downs[vote_account.as_str()] {
                let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                    vote_account,
                    &DOWN,
                    &snapshot_epoch,
                    &snapshot_created_at,
                    &default_status_end_at,
                    credits_total,
                ];
                query.add(&mut params);
                warn!("Validator {vote_account} is now DOWN");
            } else {
                let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                    vote_account,
                    &UP,
                    &snapshot_epoch,
                    &snapshot_created_at,
                    &default_status_end_at,
                    credits_total,
                ];
                query.add(&mut params);
                info!("Validator {vote_account} is now UP");
            }
        }
    }
    let insertions = query.execute(psql_client).await?;
    info!("Stored {} changed uptimes", insertions.unwrap_or(0));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn performance(
        delinquent: bool,
        last_vote: Option<u64>,
        credits_total: Option<u64>,
    ) -> ValidatorPerformance {
        ValidatorPerformance {
            commission: 0,
            version: None,
            client_id: None,
            client_id_raw: None,
            feature_set: None,
            shred_version: None,
            credits: None,
            credits_regime: None,
            alpenglow_credits: None,
            last_vote,
            credits_total,
            leader_slots: 0,
            blocks_produced: 0,
            skip_rate: 0.0,
            delinquent,
        }
    }

    #[test]
    fn a_voting_validator_keeps_the_rpc_delinquency() {
        assert!(is_down(
            &performance(true, Some(446897992), Some(10)),
            Some(Decimal::from(5))
        ));
        assert!(!is_down(
            &performance(false, Some(446897992), Some(10)),
            Some(Decimal::from(10))
        ));
    }

    #[test]
    fn without_votes_growing_credits_is_up() {
        assert!(!is_down(
            &performance(true, Some(0), Some(11)),
            Some(Decimal::from(10))
        ));
        assert!(!is_down(
            &performance(true, None, Some(11)),
            Some(Decimal::from(10))
        ));
    }

    #[test]
    fn without_votes_flat_credits_is_down() {
        assert!(is_down(
            &performance(false, Some(0), Some(10)),
            Some(Decimal::from(10))
        ));
    }

    #[test]
    fn without_votes_and_no_previous_credits_keeps_the_rpc_delinquency() {
        assert!(!is_down(&performance(false, Some(0), Some(10)), None));
        assert!(is_down(
            &performance(true, Some(0), None),
            Some(Decimal::from(10))
        ));
    }
}
