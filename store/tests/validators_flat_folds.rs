use chrono::{DateTime, Utc};
use store::docs::{VersionSample, VersionState};
use store::dto::Validator;
use store::utils::load_validators_aggregated_flat;
use store::warehouse::Warehouse;

mod common;

const LAST_EPOCH: u64 = 1000;
const EPOCHS: u64 = 7;
const VOTE_ACCOUNT: &str = "voteA";

fn store_validator(warehouse: &mut Warehouse, epoch: u64, activated_stake: u64, credits: u64) {
    let validator = Validator {
        activated_stake: activated_stake.into(),
        credits: Some(credits.into()),
        leader_slots: 100.into(),
        blocks_produced: 100.into(),
        ..common::validator(VOTE_ACCOUNT, epoch)
    };
    warehouse
        .snapshots
        .entry(epoch)
        .or_default()
        .insert(VOTE_ACCOUNT.to_string(), validator);
}

fn store_version(
    warehouse: &mut Warehouse,
    epoch: u64,
    created_at: &str,
    version: Option<&str>,
    client_id: Option<i32>,
    client_id_raw: Option<&str>,
) {
    let sample = VersionSample {
        epoch,
        epoch_slot: 0,
        version: version.map(str::to_string),
        client_id,
        client_id_raw: client_id_raw.map(str::to_string),
        feature_set: None,
        shred_version: None,
        created_at: created_at.parse::<DateTime<Utc>>().expect("timestamp"),
    };
    match warehouse.live.versions.get_mut(VOTE_ACCOUNT) {
        Some(state) => {
            state.changes.push(sample.clone());
            state.last = sample;
        }
        None => {
            warehouse.live.versions.insert(
                VOTE_ACCOUNT.to_string(),
                VersionState {
                    last: sample.clone(),
                    changes: vec![sample],
                },
            );
        }
    }
}

fn warehouse_with_window() -> Warehouse {
    let mut warehouse = Warehouse::default();
    for epoch in (LAST_EPOCH - EPOCHS + 1)..=LAST_EPOCH {
        store_validator(&mut warehouse, epoch, 100, 10);
    }
    warehouse
}

#[test]
fn validators_flat_reports_unknown_for_a_client_the_registry_does_not_know() {
    let mut warehouse = warehouse_with_window();
    store_version(
        &mut warehouse,
        900,
        "2026-01-01T00:00:00Z",
        Some("2.0.0"),
        None,
        Some("Raiku2"),
    );

    let validators = load_validators_aggregated_flat(&warehouse, LAST_EPOCH, EPOCHS).expect("flat");
    assert_eq!(validators.len(), 1);
    assert_eq!(
        validators[0].client_vendor, "unknown",
        "a rendering the registry cannot resolve must not classify the validator"
    );
    assert_eq!(validators[0].client_lineage, "unknown");
}

#[test]
fn validators_flat_classifies_from_the_raw_rendering_when_no_id_was_stored() {
    let mut warehouse = warehouse_with_window();
    store_version(
        &mut warehouse,
        900,
        "2026-01-01T00:00:00Z",
        Some("2.0.0"),
        None,
        Some("AgaveBam"),
    );

    let validators = load_validators_aggregated_flat(&warehouse, LAST_EPOCH, EPOCHS).expect("flat");
    assert_eq!(validators.len(), 1);
    assert_eq!(validators[0].client_vendor, "bam");
    assert_eq!(validators[0].client_lineage, "agave");
}

// The two aggregates must read the same version change, or a stale id outranks a newer rendering.
#[test]
fn validators_flat_takes_id_and_rendering_from_the_same_change() {
    let mut warehouse = warehouse_with_window();
    store_version(
        &mut warehouse,
        900,
        "2026-01-01T00:00:00Z",
        Some("2.0.0"),
        Some(5),
        Some("Firedancer"),
    );
    store_version(
        &mut warehouse,
        950,
        "2026-02-01T00:00:00Z",
        Some("2.1.0"),
        None,
        Some("Raiku2"),
    );

    let validators = load_validators_aggregated_flat(&warehouse, LAST_EPOCH, EPOCHS).expect("flat");
    assert_eq!(validators.len(), 1);
    assert_eq!(
        validators[0].client_vendor, "unknown",
        "the newest change reports an unresolvable client, so the older firedancer id must not win"
    );
    assert_eq!(validators[0].client_lineage, "unknown");
}

#[test]
fn validators_flat_client_columns_keep_open_lower_bound_and_bounded_upper_bound() {
    let mut warehouse = warehouse_with_window();
    store_version(
        &mut warehouse,
        900,
        "2026-01-01T00:00:00Z",
        Some("2.0.0"),
        Some(6),
        None,
    );
    store_version(
        &mut warehouse,
        999,
        "2026-02-01T00:00:00Z",
        Some("2.1.0"),
        None,
        None,
    );
    store_version(
        &mut warehouse,
        1001,
        "2026-03-01T00:00:00Z",
        Some("2.2.0"),
        Some(5),
        None,
    );

    let validators = load_validators_aggregated_flat(&warehouse, LAST_EPOCH, EPOCHS).expect("flat");
    assert_eq!(validators.len(), 1);
    let validator = &validators[0];

    assert_eq!(
        validator.client_vendor, "bam",
        "the only client change sits below the epoch window, so the lower bound must stay open"
    );
    assert_eq!(validator.client_lineage, "agave");
    assert_eq!(
        validator.version, "2.2.0",
        "last_version is deliberately unbounded and still reports data from above last_epoch"
    );
}

#[test]
fn validators_flat_survives_zero_epochs() {
    let mut warehouse = Warehouse::default();
    store_validator(&mut warehouse, LAST_EPOCH, 100, 10);

    let validators = load_validators_aggregated_flat(&warehouse, LAST_EPOCH, 0).expect("flat");
    assert!(
        validators.is_empty(),
        "epochs=0 can never satisfy the epochs-with-credits floor"
    );
}

#[test]
fn validators_flat_averages_the_window_it_is_given() {
    let mut warehouse = Warehouse::default();
    for epoch in (LAST_EPOCH - EPOCHS + 1)..=LAST_EPOCH {
        store_validator(&mut warehouse, epoch, 2_000_000_000, 100);
    }
    store_version(
        &mut warehouse,
        LAST_EPOCH,
        "2026-03-01T00:00:00Z",
        Some("2.2.0"),
        Some(3),
        None,
    );

    let validators = load_validators_aggregated_flat(&warehouse, LAST_EPOCH, EPOCHS).expect("flat");
    let validator = &validators[0];
    assert_eq!(validator.minimum_stake, 2f64);
    assert_eq!(validator.avg_stake, 2f64);
    assert_eq!(
        validator.max_commission, 100,
        "an unknown commission counts as the worst one"
    );
    assert_eq!(
        validator.avg_adjusted_credits, 0f64,
        "credits adjusted by a 100% commission are worth nothing"
    );
    assert_eq!(validator.dc_aso, "Unknown");
}

#[test]
fn validators_flat_grace_average_ignores_an_epoch_without_any_known_rate() {
    let mut warehouse = warehouse_with_window();
    for (epoch, snapshot) in warehouse.snapshots.iter_mut() {
        let validator = snapshot.get_mut(VOTE_ACCOUNT).unwrap();
        validator.skip_rate = if *epoch == LAST_EPOCH - EPOCHS + 1 {
            None
        } else {
            Some(0.2)
        };
    }

    let validators = load_validators_aggregated_flat(&warehouse, LAST_EPOCH, EPOCHS).unwrap();
    assert_eq!(validators.len(), 1);
    assert!((validators[0].avg_skip_rate - 0.2).abs() < 1e-12);
    assert!((validators[0].avg_grace_skip_rate - 0.2).abs() < 1e-12);

    for snapshot in warehouse.snapshots.values_mut() {
        snapshot.get_mut(VOTE_ACCOUNT).unwrap().skip_rate = None;
    }
    let validators = load_validators_aggregated_flat(&warehouse, LAST_EPOCH, EPOCHS).unwrap();
    assert_eq!(validators.len(), 1);
    assert_eq!(validators[0].avg_skip_rate, 1.0);
    assert_eq!(validators[0].avg_grace_skip_rate, 1.0);
}

#[test]
fn validators_flat_reads_an_unknown_skip_rate_as_unknown() {
    let mut warehouse = Warehouse::default();
    for epoch in (LAST_EPOCH - EPOCHS + 1)..=LAST_EPOCH {
        let partly_skip_rate = (epoch != LAST_EPOCH).then_some(0.1);
        for (vote_account, stake, leader_slots, blocks_produced, skip_rate) in [
            ("graced", 100, 100, 50, Some(0.5)),
            ("steady", 100, 300, 240, Some(0.2)),
            ("unknown", 1000, 0, 0, None),
            ("partly", 100, 300, 270, partly_skip_rate),
        ] {
            let validator = Validator {
                activated_stake: stake.into(),
                credits: Some(10.into()),
                leader_slots: leader_slots.into(),
                blocks_produced: blocks_produced.into(),
                skip_rate,
                ..common::validator(vote_account, epoch)
            };
            warehouse
                .snapshots
                .entry(epoch)
                .or_default()
                .insert(vote_account.to_string(), validator);
        }
    }

    let validators = load_validators_aggregated_flat(&warehouse, LAST_EPOCH, EPOCHS).unwrap();
    assert_eq!(validators.len(), 4);
    let flat = |vote_account: &str| {
        validators
            .iter()
            .find(|v| v.vote_account == vote_account)
            .unwrap()
    };
    let close = |actual: f64, expected: f64| (actual - expected).abs() < 1e-9;
    let cluster_with_partly = (0.5 * 100.0 + 0.2 * 100.0 + 0.1 * 100.0) / 300.0;
    let cluster_without_partly = (0.5 * 100.0 + 0.2 * 100.0) / 200.0;
    let graced_expected =
        ((EPOCHS - 1) as f64 * cluster_with_partly + cluster_without_partly) / EPOCHS as f64;
    let partly_grace_expected =
        ((EPOCHS - 1) as f64 * 0.1 + cluster_without_partly) / EPOCHS as f64;

    assert!(close(flat("partly").avg_skip_rate, 0.1));
    assert!(close(
        flat("partly").avg_grace_skip_rate,
        partly_grace_expected
    ));
    assert!(close(flat("graced").avg_skip_rate, 0.5));
    assert!(close(flat("graced").avg_grace_skip_rate, graced_expected));
    assert!(close(flat("steady").avg_grace_skip_rate, 0.2));
    assert_eq!(flat("unknown").avg_skip_rate, 1.0);
    assert!(close(flat("unknown").avg_grace_skip_rate, graced_expected));
}

#[test]
fn validators_flat_reads_the_lineage_of_id_12_off_the_same_versions_row() {
    for (version, expected_lineage) in [("0.1204.40300", "frankendancer"), ("26.9.4", "firedancer")]
    {
        let mut warehouse = warehouse_with_window();
        store_version(
            &mut warehouse,
            990,
            "2026-01-01T00:00:00Z",
            Some(version),
            Some(12),
            None,
        );
        store_version(
            &mut warehouse,
            LAST_EPOCH + 1,
            "2026-03-01T00:00:00Z",
            Some("26.9.4"),
            Some(12),
            None,
        );

        let validators = load_validators_aggregated_flat(&warehouse, LAST_EPOCH, EPOCHS).unwrap();
        assert_eq!(validators.len(), 1);
        assert_eq!(validators[0].client_lineage, expected_lineage);
        assert_eq!(validators[0].client_vendor, "bam");
        assert_eq!(validators[0].version, "26.9.4");
    }
}
