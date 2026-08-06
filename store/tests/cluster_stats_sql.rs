mod common;

use chrono::{DateTime, Utc};
use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use store::utils::{
    load_client_diversity_stats, load_client_lineage_stats, load_dc_concentration_stats,
    load_validators_aggregated_flat,
};
use tokio_postgres::Client;

const LAST_EPOCH: u64 = 1000;
const EPOCHS: u64 = 7;
const GAP_EPOCH: u64 = 997;

async fn insert_validator(
    client: &Client,
    vote_account: &str,
    epoch: u64,
    activated_stake: u64,
    credits: u64,
    client_id: Option<i32>,
) {
    insert_validator_raw(
        client,
        vote_account,
        epoch,
        activated_stake,
        credits,
        client_id,
        None,
    )
    .await;
}

async fn insert_validator_raw(
    client: &Client,
    vote_account: &str,
    epoch: u64,
    activated_stake: u64,
    credits: u64,
    client_id: Option<i32>,
    client_id_raw: Option<&str>,
) {
    client
        .execute(
            "INSERT INTO validators (
                identity, vote_account, epoch, activated_stake, marinade_stake,
                marinade_native_stake, superminority, stake_to_become_superminority, credits,
                leader_slots, blocks_produced, skip_rate, client_id, client_id_raw, updated_at
            ) VALUES ($1, $2, $3, $4, 0, 0, false, 0, $5, 100, 100, 0, $6, $7, NOW())",
            &[
                &format!("identity-{vote_account}"),
                &vote_account,
                &Decimal::from(epoch),
                &Decimal::from(activated_stake),
                &Decimal::from(credits),
                &client_id,
                &client_id_raw,
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
    client_id: Option<i32>,
) {
    insert_version_raw(
        client,
        vote_account,
        epoch,
        created_at,
        version,
        client_id,
        None,
    )
    .await;
}

async fn insert_version_raw(
    client: &Client,
    vote_account: &str,
    epoch: u64,
    created_at: &str,
    version: Option<&str>,
    client_id: Option<i32>,
    client_id_raw: Option<&str>,
) {
    client
        .execute(
            "INSERT INTO versions (
                vote_account, epoch, epoch_slot, version, client_id, client_id_raw, created_at
            ) VALUES ($1, $2, 0, $3, $4, $5, $6)",
            &[
                &vote_account,
                &Decimal::from(epoch),
                &version,
                &client_id,
                &client_id_raw,
                &created_at.parse::<DateTime<Utc>>().unwrap(),
            ],
        )
        .await
        .unwrap();
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
        insert_validator(&client, "voteA", epoch, 100, 10, Some(1)).await;
        insert_validator(&client, "voteB", epoch, 200, 10, Some(1)).await;
        insert_validator(&client, "voteC", epoch, 700, 10, None).await;
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

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

// Ids 9, 10 and 11 are the three lineages harmonic ships.
#[tokio::test]
async fn client_diversity_merges_the_ids_a_vendor_ships_across_lineages() {
    let schema = "ds_test_client_diversity_vendor_merge";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    for (vote_account, client_id, stake) in [
        ("voteHarmonicFiredancer", 9, 100),
        ("voteHarmonicAgave", 10, 200),
        ("voteHarmonicFrankendancer", 11, 300),
        ("voteAgave", 3, 400),
    ] {
        insert_validator(
            &client,
            vote_account,
            LAST_EPOCH,
            stake,
            10,
            Some(client_id),
        )
        .await;
    }

    let diversity = load_client_diversity_stats(&client, 1).await.unwrap();
    let stats = diversity
        .iter()
        .find(|stats| stats.epoch == LAST_EPOCH)
        .unwrap();

    assert_eq!(stats.total_activated_stake, 1000);
    assert_eq!(
        stats.client_stake.get("harmonic"),
        Some(&600),
        "all three harmonic ids contribute to one bucket"
    );
    assert_eq!(
        stats.client_validator_count.get("harmonic"),
        Some(&3),
        "the validator count merges alongside the stake"
    );
    assert!((stats.client_share.get("harmonic").unwrap() - 0.6).abs() < 1e-9);
    assert_eq!(stats.client_stake.get("agave"), Some(&400));

    let lineage = load_client_lineage_stats(&client, 1).await.unwrap();
    let stats = lineage
        .iter()
        .find(|stats| stats.epoch == LAST_EPOCH)
        .unwrap();
    assert_eq!(
        stats.lineage_stake.get("agave"),
        Some(&600),
        "id 10 is an agave fork, so it merges with plain agave on the lineage axis"
    );
    assert_eq!(stats.lineage_stake.get("firedancer"), Some(&100));
    assert_eq!(stats.lineage_stake.get("frankendancer"), Some(&300));

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn client_diversity_classifies_a_validator_from_its_raw_rendering_alone() {
    let schema = "ds_test_client_diversity_raw";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    insert_validator_raw(
        &client,
        "voteStored",
        LAST_EPOCH,
        100,
        10,
        Some(1),
        Some("JitoLabs"),
    )
    .await;
    insert_validator_raw(
        &client,
        "voteRawName",
        LAST_EPOCH,
        200,
        10,
        None,
        Some("JitoLabs"),
    )
    .await;
    insert_validator_raw(
        &client,
        "voteRawNumber",
        LAST_EPOCH,
        300,
        10,
        None,
        Some("Unknown(1)"),
    )
    .await;
    insert_validator_raw(
        &client,
        "voteUnregistered",
        LAST_EPOCH,
        400,
        10,
        None,
        Some("Raiku2"),
    )
    .await;
    insert_validator(&client, "voteNoClient", LAST_EPOCH, 500, 10, None).await;

    let diversity = load_client_diversity_stats(&client, 1).await.unwrap();
    let stats = diversity
        .iter()
        .find(|stats| stats.epoch == LAST_EPOCH)
        .unwrap();

    assert_eq!(stats.total_activated_stake, 1500);
    assert_eq!(
        stats.client_stake.get("jito"),
        Some(&600),
        "a stored id and both raw renderings of the same client land in one bucket"
    );
    assert_eq!(stats.client_validator_count.get("jito"), Some(&3));
    assert_eq!(
        stats.client_stake.get("unknown"),
        Some(&900),
        "an unregistered rendering and no client at all both stay unclassified"
    );
    assert_eq!(stats.client_validator_count.get("unknown"), Some(&2));

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn validators_flat_reports_unknown_for_a_client_the_registry_does_not_know() {
    let schema = "ds_test_validators_flat_unknown_client";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    for epoch in (LAST_EPOCH - EPOCHS + 1)..=LAST_EPOCH {
        insert_validator(&client, "voteA", epoch, 100, 10, None).await;
    }
    insert_version_raw(
        &client,
        "voteA",
        900,
        "2026-01-01T00:00:00Z",
        Some("2.0.0"),
        None,
        Some("Raiku2"),
    )
    .await;

    let validators = load_validators_aggregated_flat(&client, LAST_EPOCH, EPOCHS)
        .await
        .unwrap();
    assert_eq!(validators.len(), 1);
    assert_eq!(
        validators[0].client_vendor, "unknown",
        "a rendering the registry cannot resolve must not classify the validator"
    );
    assert_eq!(validators[0].client_lineage, "unknown");

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn validators_flat_classifies_from_the_raw_rendering_when_no_id_was_stored() {
    let schema = "ds_test_validators_flat_raw_client";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    for epoch in (LAST_EPOCH - EPOCHS + 1)..=LAST_EPOCH {
        insert_validator(&client, "voteA", epoch, 100, 10, None).await;
    }
    insert_version_raw(
        &client,
        "voteA",
        900,
        "2026-01-01T00:00:00Z",
        Some("2.0.0"),
        None,
        Some("AgaveBam"),
    )
    .await;

    let validators = load_validators_aggregated_flat(&client, LAST_EPOCH, EPOCHS)
        .await
        .unwrap();
    assert_eq!(validators.len(), 1);
    assert_eq!(validators[0].client_vendor, "bam");
    assert_eq!(validators[0].client_lineage, "agave");

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

// The two aggregates must read the same versions row, or a stale id outranks a newer rendering.
#[tokio::test]
async fn validators_flat_takes_id_and_rendering_from_the_same_versions_row() {
    let schema = "ds_test_validators_flat_same_row";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    for epoch in (LAST_EPOCH - EPOCHS + 1)..=LAST_EPOCH {
        insert_validator(&client, "voteA", epoch, 100, 10, None).await;
    }
    insert_version_raw(
        &client,
        "voteA",
        900,
        "2026-01-01T00:00:00Z",
        Some("2.0.0"),
        Some(5),
        Some("Firedancer"),
    )
    .await;
    insert_version_raw(
        &client,
        "voteA",
        950,
        "2026-02-01T00:00:00Z",
        Some("2.1.0"),
        None,
        Some("Raiku2"),
    )
    .await;

    let validators = load_validators_aggregated_flat(&client, LAST_EPOCH, EPOCHS)
        .await
        .unwrap();
    assert_eq!(validators.len(), 1);
    assert_eq!(
        validators[0].client_vendor, "unknown",
        "the newest row reports an unresolvable client, so the older firedancer id must not win"
    );
    assert_eq!(validators[0].client_lineage, "unknown");

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
        insert_validator(&client, "voteA", epoch, 100, 10, Some(1)).await;
    }

    insert_version(
        &client,
        "voteA",
        900,
        "2026-01-01T00:00:00Z",
        Some("2.0.0"),
        Some(6),
    )
    .await;
    insert_version(
        &client,
        "voteA",
        999,
        "2026-02-01T00:00:00Z",
        Some("2.1.0"),
        None,
    )
    .await;
    insert_version(
        &client,
        "voteA",
        1001,
        "2026-03-01T00:00:00Z",
        Some("2.2.0"),
        Some(5),
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

#[tokio::test]
async fn validators_flat_survives_zero_epochs() {
    let schema = "ds_test_validators_flat_zero";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    insert_validator(&client, "voteA", LAST_EPOCH, 100, 10, Some(1)).await;

    let validators = load_validators_aggregated_flat(&client, LAST_EPOCH, 0)
        .await
        .unwrap();
    assert!(
        validators.is_empty(),
        "epochs=0 can never satisfy the HAVING clause"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
