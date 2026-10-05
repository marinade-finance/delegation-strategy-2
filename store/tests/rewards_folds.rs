use chrono::{DateTime, Utc};
use std::collections::HashMap;
use store::docs::{ClusterInfoSample, EpochDoc};
use store::rewards::{get_estimated_inflation_rewards, get_running_epoch_slots_per_year};
use store::warehouse::Warehouse;

const SUPPLY_LAMPORTS: u64 = 600_000_000_000_000_000;
const INFLATION: f64 = 0.043;

const BASELINE_SLOTS_PER_YEAR: f64 = 78_892_314.984;
const SLOTS_PER_YEAR_350MS: f64 = 90_162_645.696;

const LEGACY_EPOCHS_PER_YEAR: f64 = 365.25 / 2.0;

fn at(moment: &str) -> DateTime<Utc> {
    moment.parse().expect("timestamp")
}

fn store_epoch(warehouse: &mut Warehouse, epoch: u64, slots_per_year: f64) {
    warehouse.epochs.insert(
        epoch,
        EpochDoc {
            epoch,
            start_at: at("2026-08-03T00:00:00Z"),
            end_at: at("2026-08-05T00:00:00Z"),
            transaction_count: 0,
            supply: SUPPLY_LAMPORTS.into(),
            inflation: INFLATION,
            inflation_taper: 0.15,
            slots_per_year,
        },
    );
}

fn sample_cluster_info(
    warehouse: &mut Warehouse,
    epoch: u64,
    epoch_slot: u64,
    slots_per_year: f64,
) {
    warehouse.live.cluster_info.epoch = epoch;
    warehouse.live.cluster_info.samples.push(ClusterInfoSample {
        epoch,
        epoch_slot,
        transaction_count: 0,
        created_at: at("2026-08-03T00:00:00Z"),
        slots_per_year,
    });
}

// Routing through a stored field is only safe if no historical figure moves.
#[test]
fn backfilled_epochs_reproduce_the_legacy_inflation_estimate() {
    let mut warehouse = Warehouse::default();
    store_epoch(&mut warehouse, 1000, BASELINE_SLOTS_PER_YEAR);
    store_epoch(&mut warehouse, 1001, SLOTS_PER_YEAR_350MS);

    let rows = get_estimated_inflation_rewards(&warehouse, 10);
    let rewards: HashMap<_, _> = rows
        .iter()
        .map(|(epoch, amount, _)| (*epoch, *amount))
        .collect();
    let provenance: HashMap<_, _> = rows
        .iter()
        .map(|(epoch, _, slots_per_year)| (*epoch, *slots_per_year))
        .collect();

    let legacy = SUPPLY_LAMPORTS as f64 * INFLATION / 1e9 / LEGACY_EPOCHS_PER_YEAR;
    let baseline = rewards[&1000];
    assert!(
        (baseline - legacy).abs() / legacy < 1e-4,
        "backfilled epoch moved: {baseline} vs {legacy}"
    );

    // Stage 1 mints 350/400 of the baseline per epoch.
    let stage_1 = rewards[&1001];
    assert!(
        (stage_1 / baseline - 350.0 / 400.0).abs() < 1e-9,
        "stage 1 epoch is {stage_1}, baseline {baseline}"
    );

    assert_eq!(provenance[&1000], BASELINE_SLOTS_PER_YEAR);
    assert_eq!(provenance[&1001], SLOTS_PER_YEAR_350MS);
}

// The transition epoch is auctioned before it closes, so its regime has to be readable while it runs.
#[test]
fn the_running_epoch_reports_its_own_regime_before_it_closes() {
    let mut warehouse = Warehouse::default();
    store_epoch(&mut warehouse, 1000, BASELINE_SLOTS_PER_YEAR);
    sample_cluster_info(&mut warehouse, 1000, 431_000, BASELINE_SLOTS_PER_YEAR);
    assert_eq!(
        get_running_epoch_slots_per_year(&warehouse),
        None,
        "a closed epoch is already covered by its own document"
    );

    sample_cluster_info(&mut warehouse, 1001, 100, SLOTS_PER_YEAR_350MS);
    sample_cluster_info(&mut warehouse, 1001, 20_000, SLOTS_PER_YEAR_350MS);
    assert_eq!(
        get_running_epoch_slots_per_year(&warehouse),
        Some((1001, SLOTS_PER_YEAR_350MS))
    );
}
