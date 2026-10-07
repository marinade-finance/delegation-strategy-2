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
