use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use store::utils::{
    load_block_production_stats, load_client_diversity_stats, load_dc_concentration_stats,
    load_validators_aggregated_flat,
};
use tokio_postgres::{Client, NoTls};

const POSTGRES_URL_ENV: &str = "DS_TEST_POSTGRES_URL";
const LAST_EPOCH: u64 = 1000;
const EPOCHS: u64 = 7;
const GAP_EPOCH: u64 = 997;

async fn migrated_client(schema: &str) -> Option<Client> {
    let url = std::env::var(POSTGRES_URL_ENV).ok()?;

    let (client, connection) = tokio_postgres::connect(&url, NoTls).await.unwrap();
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            panic!("postgres connection error: {err}");
        }
    });

    client
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {schema} CASCADE;
             CREATE SCHEMA {schema};
             SET search_path TO {schema}"
        ))
        .await
        .unwrap();

    let migrations_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../migrations");
    let mut migrations: Vec<_> = std::fs::read_dir(migrations_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "sql"))
        .collect();
    migrations.sort();
    for migration in migrations {
        let sql = std::fs::read_to_string(&migration).unwrap();
        client
            .batch_execute(&sql)
            .await
            .unwrap_or_else(|err| panic!("migration {} failed: {err}", migration.display()));
    }

    Some(client)
}

async fn insert_validator(
    client: &Client,
    vote_account: &str,
    epoch: u64,
    activated_stake: u64,
    credits: u64,
    client_vendor: Option<&str>,
    client_lineage: Option<&str>,
) {
    insert_validator_in_dc(
        client,
        vote_account,
        epoch,
        activated_stake,
        credits,
        client_vendor,
        client_lineage,
        None,
        0.0,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn insert_validator_in_dc(
    client: &Client,
    vote_account: &str,
    epoch: u64,
    activated_stake: u64,
    credits: u64,
    client_vendor: Option<&str>,
    client_lineage: Option<&str>,
    dc_aso: Option<&str>,
    skip_rate: f64,
) {
    client
        .execute(
            "INSERT INTO validators (
                identity, vote_account, epoch, activated_stake, marinade_stake,
                marinade_native_stake, superminority, stake_to_become_superminority, credits,
                leader_slots, blocks_produced, skip_rate, client_vendor, client_lineage,
                dc_aso, updated_at
            ) VALUES ($1, $2, $3, $4, 0, 0, false, 0, $5, 100, 100, $8, $6, $7, $9, NOW())",
            &[
                &format!("identity-{vote_account}"),
                &vote_account,
                &Decimal::from(epoch),
                &Decimal::from(activated_stake),
                &Decimal::from(credits),
                &client_vendor,
                &client_lineage,
                &skip_rate,
                &dc_aso,
            ],
        )
        .await
        .unwrap();
}

async fn insert_version(
    client: &Client,
    vote_account: &str,
    epoch: u64,
    created_at: &str,
    version: Option<&str>,
    client_vendor: Option<&str>,
    client_lineage: Option<&str>,
) {
    client
        .execute(
            "INSERT INTO versions (
                vote_account, epoch, epoch_slot, version, client_vendor, client_lineage, created_at
            ) VALUES ($1, $2, 0, $3, $4, $5, $6)",
            &[
                &vote_account,
                &Decimal::from(epoch),
                &version,
                &client_vendor,
                &client_lineage,
                &created_at.parse::<DateTime<Utc>>().unwrap(),
            ],
        )
        .await
        .unwrap();
}

fn skip_without_database(schema: &str) -> bool {
    if std::env::var(POSTGRES_URL_ENV).is_ok() {
        return false;
    }
    eprintln!("skipping {schema}: {POSTGRES_URL_ENV} is not set");
    true
}

#[tokio::test]
async fn stake_distribution_emits_every_requested_epoch_including_gaps() {
    let schema = "ds_test_stake_distribution";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    let first_epoch = LAST_EPOCH - EPOCHS + 1;
    for epoch in first_epoch..=LAST_EPOCH {
        if epoch == GAP_EPOCH {
            continue;
        }
        insert_validator(
            &client,
            "voteA",
            epoch,
            100,
            10,
            Some("jito"),
            Some("agave"),
        )
        .await;
        insert_validator(
            &client,
            "voteB",
            epoch,
            200,
            10,
            Some("jito"),
            Some("agave"),
        )
        .await;
        insert_validator(&client, "voteC", epoch, 700, 10, None, None).await;
    }

    let diversity = load_client_diversity_stats(&client, EPOCHS).await.unwrap();

    let epochs: Vec<u64> = diversity.iter().map(|stats| stats.epoch).collect();
    assert_eq!(
        epochs,
        (first_epoch..=LAST_EPOCH).rev().collect::<Vec<u64>>(),
        "every requested epoch must be emitted exactly once, newest first"
    );

    let gap = diversity
        .iter()
        .find(|stats| stats.epoch == GAP_EPOCH)
        .unwrap();
    assert_eq!(gap.total_activated_stake, 0);
    assert!(gap.client_stake.is_empty());
    assert!(gap.client_share.is_empty());
    assert!(gap.client_validator_count.is_empty());

    let populated = diversity
        .iter()
        .find(|stats| stats.epoch == LAST_EPOCH)
        .unwrap();
    assert_eq!(populated.total_activated_stake, 1000);
    assert_eq!(populated.client_stake.get("jito"), Some(&300));
    assert_eq!(populated.client_stake.get("unknown"), Some(&700));
    assert_eq!(populated.client_validator_count.get("jito"), Some(&2));
    assert_eq!(populated.client_validator_count.get("unknown"), Some(&1));
    assert!((populated.client_share.get("jito").unwrap() - 0.3).abs() < 1e-9);
    assert!((populated.client_share.get("unknown").unwrap() - 0.7).abs() < 1e-9);

    let concentration = load_dc_concentration_stats(&client, EPOCHS).await.unwrap();
    assert_eq!(
        concentration
            .iter()
            .map(|stats| stats.epoch)
            .collect::<Vec<u64>>(),
        epochs,
        "cluster-stats series must cover the same epoch window"
    );

    let block_production = load_block_production_stats(&client, EPOCHS).await.unwrap();
    assert_eq!(
        block_production
            .iter()
            .map(|stats| stats.epoch)
            .collect::<Vec<u64>>(),
        epochs,
        "cluster-stats series must cover the same epoch window"
    );

    let gap_production = block_production
        .iter()
        .find(|stats| stats.epoch == GAP_EPOCH)
        .unwrap();
    assert_eq!(gap_production.blocks_produced, 0);
    assert_eq!(gap_production.leader_slots, 0);
    assert_eq!(gap_production.avg_skip_rate, 1.0);

    let oldest_production = block_production
        .iter()
        .find(|stats| stats.epoch == first_epoch)
        .unwrap();
    assert_eq!(
        oldest_production.leader_slots, 300,
        "the oldest requested epoch must be inside the window, not excluded by an exclusive bound"
    );
    assert_eq!(oldest_production.blocks_produced, 300);
    assert_eq!(oldest_production.avg_skip_rate, 0.0);

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn validators_flat_client_columns_keep_open_lower_bound_and_bounded_upper_bound() {
    let schema = "ds_test_validators_flat";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    for epoch in (LAST_EPOCH - EPOCHS + 1)..=LAST_EPOCH {
        insert_validator(
            &client,
            "voteA",
            epoch,
            100,
            10,
            Some("jito"),
            Some("agave"),
        )
        .await;
    }

    insert_version(
        &client,
        "voteA",
        900,
        "2026-01-01T00:00:00Z",
        Some("2.0.0"),
        Some("bam"),
        Some("agave"),
    )
    .await;
    insert_version(
        &client,
        "voteA",
        999,
        "2026-02-01T00:00:00Z",
        Some("2.1.0"),
        None,
        None,
    )
    .await;
    insert_version(
        &client,
        "voteA",
        1001,
        "2026-03-01T00:00:00Z",
        Some("2.2.0"),
        Some("firedancer-vendor"),
        Some("firedancer"),
    )
    .await;

    let validators = load_validators_aggregated_flat(&client, LAST_EPOCH, EPOCHS)
        .await
        .unwrap();
    assert_eq!(validators.len(), 1);
    let validator = &validators[0];

    assert_eq!(
        validator.client_vendor, "bam",
        "the only client row sits below the epoch window, so the lower bound must stay open"
    );
    assert_eq!(validator.client_lineage, "agave");
    assert_eq!(
        validator.version, "2.2.0",
        "last_version is deliberately unbounded and still reports data from above last_epoch"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

// the cluster_stake / cluster_skip_rate / dc CTEs are bounded to the epoch window; an epoch
// outside it must not reach any of the three aggregates
#[tokio::test]
async fn validators_flat_ignores_rows_outside_the_epoch_window() {
    let schema = "ds_test_validators_flat_window";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    let first_epoch = LAST_EPOCH - EPOCHS + 1;
    for epoch in first_epoch..=LAST_EPOCH {
        insert_validator_in_dc(
            &client,
            "voteA",
            epoch,
            100,
            10,
            None,
            None,
            Some("aso1"),
            0.5,
        )
        .await;
        insert_validator_in_dc(
            &client,
            "voteB",
            epoch,
            300,
            10,
            None,
            None,
            Some("aso2"),
            0.1,
        )
        .await;
    }

    let outside = first_epoch - 1;
    insert_validator_in_dc(
        &client,
        "voteA",
        outside,
        10_000,
        10,
        None,
        None,
        Some("aso1"),
        0.9,
    )
    .await;
    insert_validator_in_dc(
        &client,
        "voteB",
        outside,
        10_000,
        10,
        None,
        None,
        Some("aso2"),
        0.9,
    )
    .await;

    let validators = load_validators_aggregated_flat(&client, LAST_EPOCH, EPOCHS)
        .await
        .unwrap();
    assert_eq!(
        validators.len(),
        2,
        "the out-of-window epoch must not add to the HAVING COUNT(*)"
    );

    let a = validators
        .iter()
        .find(|v| v.vote_account == "voteA")
        .unwrap();

    // 100 of 400 in-window stake per epoch; the epoch-990 row would move this to 10100/20400
    assert!(
        (a.avg_dc_concentration - 0.25).abs() < 1e-9,
        "avg_dc_concentration was {}",
        a.avg_dc_concentration
    );

    // leader_slots is 100 (< 200), so grace uses least(own skip rate, cluster weighted) =
    // least(0.5, (0.5*100 + 0.1*300) / 400) = 0.2
    assert!(
        (a.avg_grace_skip_rate - 0.2).abs() < 1e-9,
        "avg_grace_skip_rate was {}",
        a.avg_grace_skip_rate
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn validators_flat_survives_zero_epochs() {
    let schema = "ds_test_validators_flat_zero";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    insert_validator(&client, "voteA", LAST_EPOCH, 100, 10, Some("jito"), None).await;

    let validators = load_validators_aggregated_flat(&client, LAST_EPOCH, 0)
        .await
        .unwrap();
    assert!(
        validators.is_empty(),
        "epochs=0 collapses to a 1-epoch window, short of the 7 positive-credit epochs HAVING requires"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
