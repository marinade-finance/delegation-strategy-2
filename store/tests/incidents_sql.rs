mod common;

use chrono::{DateTime, Utc};
use common::{migrated_client, skip_without_database};
use std::collections::HashMap;
use store::dto::{IncidentDetail, ValidatorEpochStats, ValidatorRecord};
use store::incidents::{IncidentFilters, ValidatorIncidents};
use store::utils::load_validator_incidents;
use tokio_postgres::Client;

/// No records, so no block production can be derived and only the query is under test.
fn no_records() -> HashMap<String, ValidatorRecord> {
    HashMap::new()
}

/// One validator that missed 8 of its 64 leader slots in the given epoch.
fn epoch_stats(vote_account: &str, epoch: u64) -> HashMap<String, ValidatorRecord> {
    records(&[vote_account], epoch)
}

/// 8 of 64 leader slots missed, for each of the given validators.
fn records(vote_accounts: &[&str], epoch: u64) -> HashMap<String, ValidatorRecord> {
    // Block production is only recorded for a closed epoch, so the boundaries have to be set as
    // the `epochs` join would set them.
    let epoch_start_at: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
    vote_accounts
        .iter()
        .map(|vote_account| {
            (
                vote_account.to_string(),
                ValidatorRecord {
                    epoch_stats: vec![ValidatorEpochStats {
                        epoch,
                        leader_slots: 64,
                        blocks_produced: 56,
                        epoch_start_at: Some(epoch_start_at),
                        epoch_end_at: Some(epoch_start_at + chrono::Duration::days(2)),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            )
        })
        .collect()
}

// `identity` and `vote_account` are the same string here; nothing the query reads distinguishes them.
async fn interval(
    client: &Client,
    vote_account: &str,
    status: &str,
    epoch: u64,
    start_at: &str,
    end_at: &str,
) {
    client
        .execute(
            "INSERT INTO uptimes (identity, vote_account, status, epoch, start_at, end_at)
             VALUES ($1, $1, $2, $3::TEXT::NUMERIC, $4::TEXT::TIMESTAMPTZ, $5::TEXT::TIMESTAMPTZ)",
            &[
                &vote_account,
                &status,
                &epoch.to_string(),
                &start_at,
                &end_at,
            ],
        )
        .await
        .unwrap();
}

async fn down(client: &Client, vote_account: &str, epoch: u64, start_at: &str, end_at: &str) {
    interval(client, vote_account, "DOWN", epoch, start_at, end_at).await
}

// `identity` and `vote_account` are the same string here, as they are for the uptime rows.
async fn commission(
    client: &Client,
    vote_account: &str,
    epoch: u64,
    epoch_slot: u64,
    commission: i32,
    created_at: &str,
) {
    client
        .execute(
            "INSERT INTO commissions (identity, vote_account, commission, epoch_slot, epoch, created_at)
             VALUES ($1, $1, $2, $3::TEXT::NUMERIC, $4::TEXT::NUMERIC, $5::TEXT::TIMESTAMPTZ)",
            &[
                &vote_account,
                &commission,
                &epoch_slot.to_string(),
                &epoch.to_string(),
                &created_at,
            ],
        )
        .await
        .unwrap();
}

/// Epoch, rate before and rate after, for each raise loaded.
fn raises(incidents: &ValidatorIncidents, vote_account: &str) -> Vec<(u64, u8, u8)> {
    incidents
        .get(vote_account)
        .expect("the validator has incident material")
        .commission_raises
        .iter()
        .map(|raise| (raise.epoch, raise.commission_before, raise.commission_after))
        .collect()
}

fn downtime_epochs(incidents: &ValidatorIncidents, vote_account: &str) -> Vec<u64> {
    incidents
        .get(vote_account)
        .expect("the validator has incident material")
        .downtimes
        .iter()
        .map(|downtime| downtime.epoch)
        .collect()
}

/// The `epochs` row the sandwich loader joins for the epoch boundaries.
async fn closed_epoch(client: &Client, epoch: u64, start_at: &str, end_at: &str) {
    client
        .execute(
            "INSERT INTO epochs (epoch, start_at, end_at, transaction_count, supply, inflation, inflation_taper, slots_per_year)
             VALUES ($1::TEXT::NUMERIC, $2::TEXT::TIMESTAMPTZ, $3::TEXT::TIMESTAMPTZ, 0, 0, 0, 0.15, 0)",
            &[&epoch.to_string(), &start_at, &end_at],
        )
        .await
        .unwrap();
}

async fn sandwiches(
    client: &Client,
    vote_account: &str,
    epoch: u64,
    blocks_produced: u64,
    blocks_with_sandwiches: u64,
    sandwich_rate_30d: f64,
    sandwich_rate_60d: Option<f64>,
) {
    client
        .execute(
            "INSERT INTO validators_sandwiches (
                epoch, vote_account, blocks_produced, blocks_with_sandwiches,
                sandwich_rate_30d, sandwich_rate_60d, created_at, updated_at
             ) VALUES ($1::TEXT::NUMERIC, $2, $3::TEXT::NUMERIC, $4::TEXT::NUMERIC, $5, $6, NOW(), NOW())",
            &[
                &epoch.to_string(),
                &vote_account,
                &blocks_produced.to_string(),
                &blocks_with_sandwiches.to_string(),
                &sandwich_rate_30d,
                &sandwich_rate_60d,
            ],
        )
        .await
        .unwrap();
}

/// Epoch and 30d rate, for each sandwich epoch loaded.
fn sandwich_epochs(incidents: &ValidatorIncidents, vote_account: &str) -> Vec<(u64, f64)> {
    incidents
        .get(vote_account)
        .expect("the validator has incident material")
        .sandwiches
        .iter()
        .map(|epoch| (epoch.epoch, epoch.sandwich_rate_30d))
        .collect()
}

#[tokio::test]
async fn sandwich_rows_are_loaded_with_the_epoch_boundaries() {
    let schema = "ds_test_incidents_sandwich_rows";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    closed_epoch(&client, 887, "2026-01-01T00:00:00Z", "2026-01-03T00:00:00Z").await;
    sandwiches(&client, "voteA", 887, 8888, 3944, 44.4, Some(31.1)).await;

    let incidents = load_validator_incidents(&client, 887, 887, &no_records())
        .await
        .unwrap();

    let loaded = &incidents
        .get("voteA")
        .expect("the validator has incident material")
        .sandwiches[0];
    assert_eq!(loaded.epoch, 887);
    assert_eq!(loaded.blocks_produced, 8888);
    assert_eq!(loaded.blocks_with_sandwiches, 3944);
    assert_eq!(loaded.sandwich_rate_30d, 44.4);
    assert_eq!(loaded.sandwich_rate_60d, Some(31.1));
    assert_eq!(
        loaded.epoch_start_at,
        "2026-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
    assert_eq!(
        loaded.epoch_end_at,
        "2026-01-03T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
}

// Epochs before 820 published no 60d rate at all.
#[tokio::test]
async fn a_missing_60d_rate_loads_as_none() {
    let schema = "ds_test_incidents_sandwich_60d";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    closed_epoch(&client, 791, "2026-01-01T00:00:00Z", "2026-01-03T00:00:00Z").await;
    sandwiches(&client, "voteA", 791, 2356, 1511, 64.1, None).await;

    let incidents = load_validator_incidents(&client, 791, 791, &no_records())
        .await
        .unwrap();

    assert_eq!(
        incidents
            .get("voteA")
            .expect("the validator has incident material")
            .sandwiches[0]
            .sandwich_rate_60d,
        None
    );
}

// The running epoch has no `epochs` row, so it carries no boundaries to report.
#[tokio::test]
async fn a_sandwich_epoch_with_no_epochs_row_is_left_out() {
    let schema = "ds_test_incidents_sandwich_open";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    closed_epoch(
        &client,
        1029,
        "2026-01-01T00:00:00Z",
        "2026-01-03T00:00:00Z",
    )
    .await;
    sandwiches(&client, "voteA", 1029, 5000, 300, 6.0, Some(5.5)).await;
    sandwiches(&client, "voteA", 1030, 5000, 350, 7.0, Some(6.5)).await;

    let incidents = load_validator_incidents(&client, 1029, 1030, &no_records())
        .await
        .unwrap();

    assert_eq!(sandwich_epochs(&incidents, "voteA"), vec![(1029, 6.0)]);
}

#[tokio::test]
async fn the_sandwich_window_is_closed_on_both_ends() {
    let schema = "ds_test_incidents_sandwich_window";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    for epoch in [999, 1000, 1001, 1002] {
        closed_epoch(
            &client,
            epoch,
            "2026-01-01T00:00:00Z",
            "2026-01-03T00:00:00Z",
        )
        .await;
        sandwiches(&client, "voteA", epoch, 5000, 300, 6.0, Some(5.5)).await;
    }

    let incidents = load_validator_incidents(&client, 1000, 1001, &no_records())
        .await
        .unwrap();

    assert_eq!(
        sandwich_epochs(&incidents, "voteA")
            .into_iter()
            .map(|(epoch, _)| epoch)
            .collect::<Vec<_>>(),
        vec![1000, 1001]
    );
}

#[tokio::test]
async fn sandwich_rows_are_keyed_by_vote_account() {
    let schema = "ds_test_incidents_sandwich_keys";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    closed_epoch(&client, 900, "2026-01-01T00:00:00Z", "2026-01-03T00:00:00Z").await;
    sandwiches(&client, "voteA", 900, 5000, 300, 6.0, Some(5.5)).await;
    sandwiches(&client, "voteB", 900, 5000, 50, 1.0, Some(0.9)).await;

    let incidents = load_validator_incidents(&client, 900, 900, &no_records())
        .await
        .unwrap();

    assert_eq!(sandwich_epochs(&incidents, "voteA"), vec![(900, 6.0)]);
    assert_eq!(sandwich_epochs(&incidents, "voteB"), vec![(900, 1.0)]);
}

#[tokio::test]
async fn every_row_of_an_epoch_gets_the_cluster_median() {
    let schema = "ds_test_incidents_sandwich_median";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    closed_epoch(&client, 900, "2026-01-01T00:00:00Z", "2026-01-03T00:00:00Z").await;
    sandwiches(&client, "voteA", 900, 5000, 300, 6.0, Some(5.5)).await;
    sandwiches(&client, "voteB", 900, 5000, 50, 1.0, Some(0.9)).await;
    sandwiches(&client, "voteC", 900, 5000, 100, 2.0, Some(1.9)).await;
    sandwiches(&client, "voteD", 900, 999, 900, 90.0, Some(90.0)).await;

    let incidents = load_validator_incidents(&client, 900, 900, &no_records())
        .await
        .unwrap();

    for vote_account in ["voteA", "voteB", "voteC", "voteD"] {
        let loaded = &incidents
            .get(vote_account)
            .expect("the validator has incident material")
            .sandwiches[0];
        assert_eq!(loaded.cluster_median_rate, 2.0, "{vote_account}");
    }
}

// `uptimes` is written every minute and `validators` hourly, so epoch 102 is a live case: a DOWN row
// above the head for the hour the validator write takes to catch up.
#[tokio::test]
async fn the_window_is_closed_on_both_ends() {
    let schema = "ds_test_incidents_window";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    for (epoch, start_at, end_at) in [
        (99, "2026-01-01T00:00:00Z", "2026-01-01T00:05:00Z"),
        (100, "2026-02-01T00:00:00Z", "2026-02-01T00:05:00Z"),
        (101, "2026-03-01T00:00:00Z", "2026-03-01T00:05:00Z"),
        (102, "2026-04-01T00:00:00Z", "2026-04-01T00:05:00Z"),
    ] {
        down(&client, "voteA", epoch, start_at, end_at).await;
    }

    let incidents = load_validator_incidents(&client, 100, 101, &no_records())
        .await
        .unwrap();

    assert_eq!(downtime_epochs(&incidents, "voteA"), vec![100, 101]);
}

#[tokio::test]
async fn an_up_interval_is_not_an_incident() {
    let schema = "ds_test_incidents_status";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    interval(
        &client,
        "voteA",
        "UP",
        100,
        "2026-01-01T00:00:00Z",
        "2026-01-02T00:00:00Z",
    )
    .await;

    assert!(load_validator_incidents(&client, 100, 100, &no_records())
        .await
        .unwrap()
        .is_empty());
}

// Every filter the API applies afterwards reads `downtime_seconds`, including the restart-noise floor.
#[tokio::test]
async fn downtime_seconds_is_the_length_of_the_interval() {
    let schema = "ds_test_incidents_downtime";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    down(
        &client,
        "voteA",
        100,
        "2026-01-01T00:00:00Z",
        "2026-01-01T00:03:20Z",
    )
    .await;

    let incidents = load_validator_incidents(&client, 100, 100, &no_records())
        .await
        .unwrap();

    assert_eq!(
        incidents.get("voteA").unwrap().downtimes[0].downtime_seconds,
        200
    );
}

#[tokio::test]
async fn every_down_row_is_loaded_oldest_first() {
    let schema = "ds_test_incidents_order";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    down(
        &client,
        "voteA",
        102,
        "2026-03-01T00:00:00Z",
        "2026-03-01T00:05:00Z",
    )
    .await;
    down(
        &client,
        "voteA",
        100,
        "2026-01-01T00:00:00Z",
        "2026-01-01T00:05:00Z",
    )
    .await;
    down(
        &client,
        "voteA",
        101,
        "2026-02-01T00:00:00Z",
        "2026-02-01T00:05:00Z",
    )
    .await;

    let incidents = load_validator_incidents(&client, 100, 102, &no_records())
        .await
        .unwrap();

    assert_eq!(downtime_epochs(&incidents, "voteA"), vec![100, 101, 102]);
}

#[tokio::test]
async fn a_closed_epoch_reports_its_block_production() {
    let schema = "ds_test_incidents_block_production";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    let incidents = load_validator_incidents(&client, 100, 100, &epoch_stats("voteA", 100))
        .await
        .unwrap();

    let production = &incidents.get("voteA").unwrap().block_production;
    assert_eq!(production.len(), 1);
    assert_eq!(production[0].leader_slots, 64);
    assert_eq!(production[0].blocks_produced, 56);
}

// The epoch in flight has no `epochs` row yet, and its counters cover only the slots so far.
#[tokio::test]
async fn the_running_epoch_reports_no_block_production() {
    let schema = "ds_test_incidents_running_epoch";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    let mut records = epoch_stats("voteA", 100);
    for record in records.values_mut() {
        record.epoch_stats[0].epoch_end_at = None;
    }

    let incidents = load_validator_incidents(&client, 100, 100, &records)
        .await
        .unwrap();

    assert!(incidents.is_empty());
}

// The window bound is the query's, so an epoch older than it is not read either.
#[tokio::test]
async fn an_epoch_before_from_epoch_is_left_out() {
    let schema = "ds_test_incidents_block_production_window";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    let incidents = load_validator_incidents(&client, 100, 100, &epoch_stats("voteA", 99))
        .await
        .unwrap();

    assert!(incidents.is_empty());
}

// The `DOWN` rows are keyed by the row's vote account and the block production by the records key,
// so a mix-up between the two would file one validator's epoch under the other.
#[tokio::test]
async fn each_validator_is_keyed_by_its_own_vote_account() {
    let schema = "ds_test_incidents_keying";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    down(
        &client,
        "voteA",
        100,
        "2026-01-01T00:00:00Z",
        "2026-01-01T00:10:00Z",
    )
    .await;

    let incidents = load_validator_incidents(&client, 100, 100, &records(&["voteA", "voteB"], 100))
        .await
        .unwrap();

    let down_only = incidents.get("voteA").unwrap();
    assert_eq!(downtime_epochs(&incidents, "voteA"), vec![100]);
    assert_eq!(down_only.block_production.len(), 1);

    let skipped_only = incidents.get("voteB").unwrap();
    assert!(skipped_only.downtimes.is_empty());
    assert_eq!(skipped_only.block_production.len(), 1);

    // Both breached, so the one that also went down reports it on its downtime record and the one
    // that stayed up reports it on a record of its own.
    let filters = IncidentFilters::default();
    assert!(matches!(
        incidents.into_response_incidents("voteA", &filters)[0].detail,
        IncidentDetail::Downtime {
            block_production: Some(_),
            ..
        }
    ));
    assert!(matches!(
        incidents.into_response_incidents("voteB", &filters)[0].detail,
        IncidentDetail::BlockProduction { .. }
    ));
}

#[tokio::test]
async fn a_raise_over_the_bar_is_loaded_with_the_rate_it_came_from() {
    let schema = "ds_test_incidents_commission_raise";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    commission(&client, "voteA", 99, 100, 5, "2026-01-01T00:00:00Z").await;
    commission(&client, "voteA", 100, 200, 100, "2026-02-01T00:00:00Z").await;

    let incidents = load_validator_incidents(&client, 100, 100, &no_records())
        .await
        .unwrap();

    assert_eq!(raises(&incidents, "voteA"), vec![(100, 5, 100)]);
    let loaded = &incidents.get("voteA").unwrap().commission_raises[0];
    assert_eq!(loaded.epoch_slot, 200);
    assert_eq!(
        loaded.changed_at,
        "2026-02-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
}

// The epoch under the window is queried for the rate the window's first epoch moved from, and for
// nothing else.
#[tokio::test]
async fn a_raise_inside_the_baseline_epoch_is_not_served() {
    let schema = "ds_test_incidents_commission_baseline";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    commission(&client, "voteA", 99, 100, 5, "2026-01-01T00:00:00Z").await;
    commission(&client, "voteA", 99, 200, 100, "2026-01-01T01:00:00Z").await;
    commission(&client, "voteA", 100, 100, 100, "2026-02-01T00:00:00Z").await;

    let incidents = load_validator_incidents(&client, 100, 100, &no_records())
        .await
        .unwrap();

    assert!(incidents.is_empty());
}

#[tokio::test]
async fn a_validator_first_sampled_over_the_bar_opens_nothing() {
    let schema = "ds_test_incidents_commission_no_baseline";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    commission(&client, "voteA", 100, 100, 100, "2026-02-01T00:00:00Z").await;
    commission(&client, "voteA", 100, 200, 100, "2026-02-01T01:00:00Z").await;

    let incidents = load_validator_incidents(&client, 100, 100, &no_records())
        .await
        .unwrap();

    assert!(incidents.is_empty());
}

// `commissions` carries a row per vote account per epoch, so every validator would reach the cache
// if the loader filed them before judging their samples.
#[tokio::test]
async fn a_validator_that_never_raised_is_not_filed_at_all() {
    let schema = "ds_test_incidents_commission_quiet";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    commission(&client, "voteA", 99, 100, 5, "2026-01-01T00:00:00Z").await;
    commission(&client, "voteA", 100, 100, 5, "2026-02-01T00:00:00Z").await;

    let incidents = load_validator_incidents(&client, 100, 100, &no_records())
        .await
        .unwrap();

    assert!(incidents.is_empty());
}

// Read in the order the rows arrive, the 5 at slot 300 would open a second raise.
#[tokio::test]
async fn rows_are_read_in_slot_order_whatever_order_they_were_written_in() {
    let schema = "ds_test_incidents_commission_slot_order";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    commission(&client, "voteA", 100, 300, 5, "2026-02-01T02:00:00Z").await;
    commission(&client, "voteA", 100, 100, 5, "2026-02-01T00:00:00Z").await;
    commission(&client, "voteA", 100, 200, 100, "2026-02-01T01:00:00Z").await;

    let incidents = load_validator_incidents(&client, 100, 100, &no_records())
        .await
        .unwrap();

    assert_eq!(raises(&incidents, "voteA"), vec![(100, 5, 100)]);
}

// Only the inflation commission is read: a validator taking every MEV tip is no spike.
#[tokio::test]
async fn a_mev_commission_over_the_bar_opens_nothing() {
    let schema = "ds_test_incidents_commission_mev";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    commission(&client, "voteA", 99, 100, 5, "2026-01-01T00:00:00Z").await;
    commission(&client, "voteA", 100, 100, 5, "2026-02-01T00:00:00Z").await;
    client
        .execute(
            "INSERT INTO mev (vote_account, mev_commission, epoch_slot, epoch, created_at)
             VALUES ($1, 10000, 100::TEXT::NUMERIC, $2::TEXT::NUMERIC, $3::TEXT::TIMESTAMPTZ)",
            &[&"voteA", &"100", &"2026-02-01T00:00:00Z"],
        )
        .await
        .unwrap();

    let incidents = load_validator_incidents(&client, 100, 100, &no_records())
        .await
        .unwrap();

    assert!(incidents.is_empty());
}

#[tokio::test]
async fn a_spike_is_served_beside_the_epoch_s_downtime() {
    let schema = "ds_test_incidents_commission_beside_downtime";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    down(
        &client,
        "voteA",
        100,
        "2026-02-01T00:00:00Z",
        "2026-02-01T00:10:00Z",
    )
    .await;
    commission(&client, "voteA", 99, 100, 5, "2026-01-01T00:00:00Z").await;
    commission(&client, "voteA", 100, 200, 100, "2026-02-01T01:00:00Z").await;

    let incidents = load_validator_incidents(&client, 100, 100, &no_records())
        .await
        .unwrap();

    let filters = IncidentFilters {
        types: None,
        ..Default::default()
    };
    let served = incidents.into_response_incidents("voteA", &filters);
    assert_eq!(served.len(), 2);
    assert!(served
        .iter()
        .any(|incident| matches!(incident.detail, IncidentDetail::CommissionSpike { .. })));
}

#[tokio::test]
async fn the_bar_is_read_at_ninety() {
    let schema = "ds_test_incidents_commission_bar";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    commission(&client, "voteA", 99, 100, 89, "2026-01-01T00:00:00Z").await;
    commission(&client, "voteA", 100, 100, 90, "2026-02-01T00:00:00Z").await;
    commission(&client, "voteB", 99, 100, 88, "2026-01-01T00:00:00Z").await;
    commission(&client, "voteB", 100, 100, 89, "2026-02-01T00:00:00Z").await;

    let incidents = load_validator_incidents(&client, 100, 100, &no_records())
        .await
        .unwrap();

    assert_eq!(raises(&incidents, "voteA"), vec![(100, 89, 90)]);
    assert!(incidents.get("voteB").is_none());
}

#[tokio::test]
async fn a_drop_back_under_the_bar_opens_a_second_raise() {
    let schema = "ds_test_incidents_commission_second_raise";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    commission(&client, "voteA", 100, 100, 5, "2026-02-01T00:00:00Z").await;
    commission(&client, "voteA", 100, 200, 100, "2026-02-01T01:00:00Z").await;
    commission(&client, "voteA", 100, 300, 5, "2026-02-01T02:00:00Z").await;
    commission(&client, "voteA", 100, 400, 100, "2026-02-01T03:00:00Z").await;

    let incidents = load_validator_incidents(&client, 100, 100, &no_records())
        .await
        .unwrap();

    assert_eq!(
        raises(&incidents, "voteA"),
        vec![(100, 5, 100), (100, 5, 100)]
    );
}

#[tokio::test]
async fn a_raise_carries_the_epoch_peak_rather_than_the_crossing_sample() {
    let schema = "ds_test_incidents_commission_peak";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    commission(&client, "voteA", 100, 100, 1, "2026-02-01T00:00:00Z").await;
    commission(&client, "voteA", 100, 200, 95, "2026-02-01T01:00:00Z").await;
    commission(&client, "voteA", 100, 300, 100, "2026-02-01T02:00:00Z").await;

    let incidents = load_validator_incidents(&client, 100, 100, &no_records())
        .await
        .unwrap();

    assert_eq!(raises(&incidents, "voteA"), vec![(100, 1, 100)]);
}

// A later epoch's higher rate is its own epoch's business, not this raise's peak.
#[tokio::test]
async fn the_peak_does_not_reach_past_the_epoch_it_was_raised_in() {
    let schema = "ds_test_incidents_commission_peak_window";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    commission(&client, "voteA", 99, 100, 1, "2026-01-01T00:00:00Z").await;
    commission(&client, "voteA", 100, 100, 95, "2026-02-01T00:00:00Z").await;
    commission(&client, "voteA", 101, 100, 100, "2026-03-01T00:00:00Z").await;

    let incidents = load_validator_incidents(&client, 100, 101, &no_records())
        .await
        .unwrap();

    assert_eq!(raises(&incidents, "voteA"), vec![(100, 1, 95)]);
}
