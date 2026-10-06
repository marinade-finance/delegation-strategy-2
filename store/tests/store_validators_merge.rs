use clap::Parser;
use collect::validators::{Snapshot, ValidatorDataCenter, ValidatorSnapshot};
use rust_decimal::Decimal;
use store::directory::Directory;
use store::docs::{epoch_doc_path, put_whole, EpochDoc, SnapshotDoc, EPOCHS_DIR, SNAPSHOT_DIR};
use store::dto::Validator;
use store::validators::{store_validators, StoreValidatorsParams};

mod common;

const EPOCH: u64 = 1000;
const VOTE_ACCOUNT: &str = "voteMerge";
const IDENTITY: &str = "identityMerge";

fn resolved_data_center(aso: &str, country: &str, asn: u32, city: &str) -> ValidatorDataCenter {
    ValidatorDataCenter {
        country: Some(country.into()),
        city: Some(city.into()),
        asn: Some(asn),
        aso: Some(aso.into()),
        ..Default::default()
    }
}

fn hetzner() -> ValidatorDataCenter {
    resolved_data_center("Hetzner", "Germany", 24940, "Nuremberg")
}

fn ovh() -> ValidatorDataCenter {
    resolved_data_center("OVH", "France", 16276, "Roubaix")
}

type StoredDataCenter = (Option<String>, Option<String>, Option<i32>, Option<String>);

fn expected(data_center: &ValidatorDataCenter) -> StoredDataCenter {
    (
        data_center.aso.clone(),
        data_center.country.clone(),
        data_center.asn.map(|asn| asn as i32),
        data_center.city.clone(),
    )
}

fn snapshot(
    epoch: u64,
    node_ip: Option<&str>,
    data_center: Option<ValidatorDataCenter>,
) -> Snapshot {
    Snapshot {
        epoch,
        created_at: "2026-07-31T00:00:00Z".into(),
        validators: vec![ValidatorSnapshot {
            node_ip: node_ip.map(Into::into),
            data_center,
            ..common::validator_snapshot(IDENTITY, VOTE_ACCOUNT, common::validator_performance())
        }],
    }
}

async fn store(directory: &Directory, name: &str, snapshot: &Snapshot) {
    let path = common::write_yaml(name, snapshot);
    store_validators(
        StoreValidatorsParams::parse_from(["store", "--snapshot-file", &path]),
        directory,
    )
    .await
    .expect("store validators");
    std::fs::remove_file(path).expect("remove snapshot");
}

async fn stored(directory: &Directory, epoch: u64) -> Option<Validator> {
    let snapshot: Option<SnapshotDoc> = directory
        .get(&epoch_doc_path(SNAPSHOT_DIR, epoch))
        .await
        .expect("get snapshot")
        .map(|doc| doc.body);
    snapshot.map(|snapshot| snapshot[VOTE_ACCOUNT].clone())
}

async fn stored_data_center(directory: &Directory, epoch: u64) -> StoredDataCenter {
    let validator = stored(directory, epoch).await.expect("stored validator");
    (
        validator.dc_aso,
        validator.dc_country,
        validator.dc_asn,
        validator.dc_city,
    )
}

// The collector runs hourly against one document per epoch, and get_data_centers reports a whois
// failure as an absent data center, so without the guard the last run of the epoch decides these.
#[tokio::test]
async fn an_unresolved_run_keeps_the_data_center_the_epoch_already_holds() {
    let Some(store_handle) = common::directory_store("dc-keep").await else {
        return;
    };
    let directory = store_handle.client();

    store(
        &directory,
        "dc-insert",
        &snapshot(EPOCH, Some("A"), Some(hetzner())),
    )
    .await;
    assert_eq!(
        stored_data_center(&directory, EPOCH).await,
        expected(&hetzner())
    );

    store(
        &directory,
        "dc-unresolved",
        &snapshot(EPOCH, Some("A"), None),
    )
    .await;
    assert_eq!(
        stored_data_center(&directory, EPOCH).await,
        expected(&hetzner()),
        "an unresolved whois lookup must not blank the epoch's known data center"
    );

    store(
        &directory,
        "dc-moved",
        &snapshot(EPOCH, Some("A"), Some(ovh())),
    )
    .await;
    assert_eq!(
        stored_data_center(&directory, EPOCH).await,
        expected(&ovh()),
        "a resolved lookup must still replace the stored data center"
    );
}

#[tokio::test]
async fn a_partial_answer_is_not_completed_from_the_previous_data_center() {
    let Some(store_handle) = common::directory_store("dc-partial").await else {
        return;
    };
    let directory = store_handle.client();

    store(
        &directory,
        "dc-full",
        &snapshot(EPOCH, Some("A"), Some(hetzner())),
    )
    .await;
    let partial = ValidatorDataCenter {
        aso: Some("OVH".into()),
        ..Default::default()
    };
    store(
        &directory,
        "dc-partial",
        &snapshot(EPOCH, Some("A"), Some(partial)),
    )
    .await;

    assert_eq!(
        stored_data_center(&directory, EPOCH).await,
        (Some("OVH".into()), None, None, None),
        "mixing a resolved answer's nulls with the previous data center would invent a location"
    );
}

#[tokio::test]
async fn the_data_center_is_dropped_when_the_node_ip_changes_inside_the_epoch() {
    let Some(store_handle) = common::directory_store("dc-ip-change").await else {
        return;
    };
    let directory = store_handle.client();

    store(
        &directory,
        "dc-a",
        &snapshot(EPOCH, Some("A"), Some(hetzner())),
    )
    .await;
    store(&directory, "dc-b", &snapshot(EPOCH, Some("B"), None)).await;

    assert_eq!(
        stored_data_center(&directory, EPOCH).await,
        (None, None, None, None),
        "a node that moved must not inherit the old address's data center"
    );
}

#[tokio::test]
async fn an_unresolved_run_carries_the_data_center_into_a_new_epoch() {
    let Some(store_handle) = common::directory_store("dc-carry").await else {
        return;
    };
    let directory = store_handle.client();

    store(
        &directory,
        "dc-e",
        &snapshot(EPOCH, Some("A"), Some(hetzner())),
    )
    .await;
    store(&directory, "dc-e1", &snapshot(EPOCH + 1, Some("A"), None)).await;
    assert_eq!(
        stored_data_center(&directory, EPOCH + 1).await,
        expected(&hetzner()),
        "the previous epoch knew where this address is"
    );

    store(&directory, "dc-e2", &snapshot(EPOCH + 2, Some("B"), None)).await;
    assert_eq!(
        stored_data_center(&directory, EPOCH + 2).await,
        (None, None, None, None),
        "a different address carries nothing over"
    );
}

#[tokio::test]
async fn the_carry_reuses_the_history_of_an_address_the_node_returns_to() {
    let Some(store_handle) = common::directory_store("dc-return").await else {
        return;
    };
    let directory = store_handle.client();

    store(
        &directory,
        "dc-e",
        &snapshot(EPOCH, Some("A"), Some(hetzner())),
    )
    .await;
    store(
        &directory,
        "dc-e1",
        &snapshot(EPOCH + 1, Some("B"), Some(ovh())),
    )
    .await;
    store(&directory, "dc-e2", &snapshot(EPOCH + 2, Some("A"), None)).await;

    assert_eq!(
        stored_data_center(&directory, EPOCH + 2).await,
        expected(&hetzner()),
        "keyed by vote account alone the newest epoch would win and reject the carry"
    );
}

#[tokio::test]
async fn the_carry_steps_over_an_epoch_that_holds_no_data_center() {
    let Some(store_handle) = common::directory_store("dc-gap").await else {
        return;
    };
    let directory = store_handle.client();

    store(
        &directory,
        "dc-e",
        &snapshot(EPOCH, Some("A"), Some(hetzner())),
    )
    .await;
    store(&directory, "dc-e1", &snapshot(EPOCH + 1, Some("B"), None)).await;
    store(&directory, "dc-e2", &snapshot(EPOCH + 2, Some("A"), None)).await;

    assert_eq!(
        stored_data_center(&directory, EPOCH + 2).await,
        expected(&hetzner()),
        "an epoch some earlier gap left empty must not propagate the gap forward"
    );
}

#[tokio::test]
async fn the_carry_does_not_undo_a_data_center_the_epoch_already_holds() {
    let Some(store_handle) = common::directory_store("dc-held").await else {
        return;
    };
    let directory = store_handle.client();

    store(
        &directory,
        "dc-e",
        &snapshot(EPOCH, Some("A"), Some(hetzner())),
    )
    .await;
    store(
        &directory,
        "dc-e1",
        &snapshot(EPOCH + 1, Some("A"), Some(ovh())),
    )
    .await;
    store(
        &directory,
        "dc-e1-unresolved",
        &snapshot(EPOCH + 1, Some("A"), None),
    )
    .await;

    assert_eq!(
        stored_data_center(&directory, EPOCH + 1).await,
        expected(&ovh()),
        "the epoch's own answer outranks the previous epoch's"
    );
}

#[tokio::test]
async fn the_carry_reaches_the_edge_of_its_window() {
    let Some(store_handle) = common::directory_store("dc-window-edge").await else {
        return;
    };
    let directory = store_handle.client();

    store(
        &directory,
        "dc-e",
        &snapshot(EPOCH, Some("A"), Some(hetzner())),
    )
    .await;
    store(
        &directory,
        "dc-edge",
        &snapshot(EPOCH + 10, Some("A"), None),
    )
    .await;

    assert_eq!(
        stored_data_center(&directory, EPOCH + 10).await,
        expected(&hetzner()),
        "ten epochs back is still evidence"
    );
}

// The reach-back is bounded: a location this old is no longer evidence of where the node runs now.
#[tokio::test]
async fn the_carry_does_not_reach_past_its_window() {
    let Some(store_handle) = common::directory_store("dc-window-beyond").await else {
        return;
    };
    let directory = store_handle.client();

    store(
        &directory,
        "dc-e",
        &snapshot(EPOCH, Some("A"), Some(hetzner())),
    )
    .await;
    store(
        &directory,
        "dc-beyond",
        &snapshot(EPOCH + 11, Some("A"), None),
    )
    .await;

    assert_eq!(
        stored_data_center(&directory, EPOCH + 11).await,
        (None, None, None, None),
        "eleven epochs back is not"
    );
}

fn v4_snapshot() -> Snapshot {
    let mut snapshot = snapshot(EPOCH, None, None);
    let validator = &mut snapshot.validators[0];
    validator.inflation_rewards_collector = Some("collectorInflation".into());
    validator.block_revenue_collector = Some("collectorBlockRevenue".into());
    // 749 is not a whole percent and 10_000 is the top of the range.
    validator.inflation_rewards_commission_bps = Some(749);
    validator.inflation_rewards_commission_bps_is_v4 = Some(true);
    validator.block_revenue_commission_bps = Some(10_000);
    validator.pending_delegator_rewards = Some(u64::MAX);
    validator.inflation_rewards_collector_owner = Some("11111111111111111111111111111111".into());
    validator.inflation_rewards_collector_lamports = Some(u64::MAX);
    validator.inflation_rewards_collector_healthy = Some(true);
    validator.block_revenue_collector_owner = None;
    validator.block_revenue_collector_lamports = Some(0);
    validator.block_revenue_collector_healthy = Some(false);
    snapshot
}

// The first store of an epoch creates the document and later ones merge into it.
#[tokio::test]
async fn vote_state_fields_survive_both_write_paths_at_full_width() {
    let Some(store_handle) = common::directory_store("vote-state").await else {
        return;
    };
    let directory = store_handle.client();

    let mut snapshot = v4_snapshot();
    store(&directory, "vote-state-insert", &snapshot).await;
    let inserted = stored(&directory, EPOCH).await.expect("stored");
    assert_eq!(
        inserted.inflation_rewards_collector.as_deref(),
        Some("collectorInflation")
    );
    assert_eq!(
        inserted.block_revenue_collector.as_deref(),
        Some("collectorBlockRevenue")
    );
    assert_eq!(inserted.inflation_rewards_commission_bps, Some(749));
    assert_eq!(inserted.inflation_rewards_commission_bps_is_v4, Some(true));
    assert_eq!(inserted.block_revenue_commission_bps, Some(10_000));
    assert_eq!(
        inserted.pending_delegator_rewards,
        Some(Decimal::from(u64::MAX)),
        "a u64 must not be narrowed"
    );
    assert_eq!(
        (
            inserted.inflation_rewards_collector_owner.as_deref(),
            inserted.inflation_rewards_collector_lamports,
            inserted.inflation_rewards_collector_healthy
        ),
        (
            Some("11111111111111111111111111111111"),
            Some(Decimal::from(u64::MAX)),
            Some(true)
        )
    );
    assert_eq!(
        (
            inserted.block_revenue_collector_owner.as_deref(),
            inserted.block_revenue_collector_lamports,
            inserted.block_revenue_collector_healthy
        ),
        (None, Some(Decimal::ZERO), Some(false)),
        "a missing collector account stores no owner and zero lamports"
    );

    // The second store of the epoch merges; the validator redirected meanwhile.
    let validator = &mut snapshot.validators[0];
    validator.inflation_rewards_collector = Some("collectorRedirected".into());
    validator.inflation_rewards_commission_bps = Some(1_001);
    validator.inflation_rewards_collector_lamports = Some(1);
    validator.inflation_rewards_collector_healthy = Some(false);
    validator.block_revenue_collector_owner = Some("11111111111111111111111111111111".into());
    validator.block_revenue_collector_lamports = Some(u64::MAX);
    validator.block_revenue_collector_healthy = Some(true);
    store(&directory, "vote-state-update", &snapshot).await;
    let updated = stored(&directory, EPOCH).await.expect("stored");
    assert_eq!(
        updated.inflation_rewards_collector.as_deref(),
        Some("collectorRedirected"),
        "the merge has to carry the new collector, not keep the first one"
    );
    assert_eq!(updated.inflation_rewards_commission_bps, Some(1_001));
    assert_eq!(
        updated.block_revenue_commission_bps,
        Some(10_000),
        "the untouched fields must survive the merge"
    );
    assert_eq!(
        (
            updated.inflation_rewards_collector_lamports,
            updated.inflation_rewards_collector_healthy
        ),
        (Some(Decimal::ONE), Some(false)),
        "the redirected collector's health replaces the first one's"
    );
    assert_eq!(
        (
            updated.block_revenue_collector_owner.as_deref(),
            updated.block_revenue_collector_lamports,
            updated.block_revenue_collector_healthy
        ),
        (
            Some("11111111111111111111111111111111"),
            Some(Decimal::from(u64::MAX)),
            Some(true)
        )
    );
}

// Overwriting on an unparsed account costs close-epoch its fallback for an unbackfillable epoch.
#[tokio::test]
async fn an_unparsed_vote_state_keeps_the_epochs_last_good_sample() {
    let Some(store_handle) = common::directory_store("vote-state-unparsed").await else {
        return;
    };
    let directory = store_handle.client();

    let mut snapshot = v4_snapshot();
    store(&directory, "vote-state-parsed", &snapshot).await;

    let validator = &mut snapshot.validators[0];
    validator.inflation_rewards_collector = None;
    validator.block_revenue_collector = None;
    validator.inflation_rewards_commission_bps = None;
    validator.inflation_rewards_commission_bps_is_v4 = None;
    validator.block_revenue_commission_bps = None;
    validator.pending_delegator_rewards = None;
    validator.inflation_rewards_collector_healthy = None;
    validator.inflation_rewards_collector_lamports = None;
    store(&directory, "vote-state-unparsed", &snapshot).await;

    let kept = stored(&directory, EPOCH).await.expect("stored");
    assert_eq!(
        kept.inflation_rewards_collector.as_deref(),
        Some("collectorInflation")
    );
    assert_eq!(kept.inflation_rewards_commission_bps, Some(749));
    assert_eq!(kept.inflation_rewards_commission_bps_is_v4, Some(true));
    assert_eq!(kept.block_revenue_commission_bps, Some(10_000));
    assert_eq!(
        (
            kept.inflation_rewards_collector_lamports,
            kept.inflation_rewards_collector_healthy
        ),
        (Some(Decimal::from(u64::MAX)), Some(true)),
        "an unparsed state must not clear what the parsed one recorded"
    );
}

// The other half: a parsed state writes its nulls through, so a conversion away from v4 sticks.
#[tokio::test]
async fn a_parsed_pre_v4_state_still_clears_the_v4_only_fields_on_merge() {
    let Some(store_handle) = common::directory_store("vote-state-pre-v4").await else {
        return;
    };
    let directory = store_handle.client();

    let mut snapshot = v4_snapshot();
    store(&directory, "vote-state-v4", &snapshot).await;

    let validator = &mut snapshot.validators[0];
    validator.inflation_rewards_collector = None;
    validator.block_revenue_collector = None;
    validator.inflation_rewards_commission_bps = Some(700);
    validator.inflation_rewards_commission_bps_is_v4 = Some(false);
    validator.block_revenue_commission_bps = None;
    validator.pending_delegator_rewards = None;
    store(&directory, "vote-state-pre-v4", &snapshot).await;

    let cleared = stored(&directory, EPOCH).await.expect("stored");
    assert_eq!(cleared.inflation_rewards_collector, None);
    assert_eq!(cleared.block_revenue_collector, None);
    assert_eq!(cleared.inflation_rewards_commission_bps, Some(700));
    assert_eq!(cleared.inflation_rewards_commission_bps_is_v4, Some(false));
    assert_eq!(cleared.block_revenue_commission_bps, None);
    assert_eq!(cleared.pending_delegator_rewards, None);
}

// The stake accounts are read on their own schedule, so a run that did not read them must not
// blank what an earlier run of the epoch stored.
#[tokio::test]
async fn pending_and_direct_stakes_merge_as_the_last_amounts_read() {
    let Some(store_handle) = common::directory_store("stakes-merge").await else {
        return;
    };
    let directory = store_handle.client();

    let mut collected = snapshot(EPOCH, None, None);
    {
        let validator = &mut collected.validators[0];
        validator.activating_stake = Some(1_000);
        validator.deactivating_stake = Some(200);
        validator.direct_stake = Some(300);
        validator.direct_activating_stake = Some(40);
        validator.direct_deactivating_stake = Some(5);
    }
    store(&directory, "stakes-read", &collected).await;

    {
        let validator = &mut collected.validators[0];
        validator.activating_stake = None;
        validator.deactivating_stake = None;
        validator.direct_stake = None;
        validator.direct_activating_stake = None;
        validator.direct_deactivating_stake = None;
    }
    store(&directory, "stakes-unread", &collected).await;
    let kept = stored(&directory, EPOCH).await.expect("stored");
    assert_eq!(
        (
            kept.activating_stake,
            kept.deactivating_stake,
            kept.direct_stake,
            kept.direct_activating_stake,
            kept.direct_deactivating_stake
        ),
        (
            Some(Decimal::from(1_000)),
            Some(Decimal::from(200)),
            Some(Decimal::from(300)),
            Some(Decimal::from(40)),
            Some(Decimal::from(5))
        ),
        "a run that read no stake accounts keeps the amounts an earlier one read"
    );

    {
        let validator = &mut collected.validators[0];
        validator.activating_stake = Some(0);
        validator.direct_stake = Some(350);
    }
    store(&directory, "stakes-reread", &collected).await;
    let reread = stored(&directory, EPOCH).await.expect("stored");
    assert_eq!(
        (reread.activating_stake, reread.direct_stake),
        (Some(Decimal::ZERO), Some(Decimal::from(350))),
        "a fresh read replaces the stored amounts, zero included"
    );

    let epoch_without = EPOCH + 1;
    store(
        &directory,
        "stakes-next",
        &snapshot(epoch_without, None, None),
    )
    .await;
    let next = stored(&directory, epoch_without).await.expect("stored");
    assert_eq!(
        (next.activating_stake, next.direct_stake),
        (None, None),
        "an epoch whose runs never read the accounts stores nothing to carry"
    );
}

// close-epoch runs once per epoch, so a snapshot landing after it would leave mid-epoch values.
#[tokio::test]
async fn a_snapshot_for_a_closed_epoch_is_skipped() {
    let Some(store_handle) = common::directory_store("closed-epoch-snapshot").await else {
        return;
    };
    let directory = store_handle.client();

    put_whole(
        &directory,
        &epoch_doc_path(EPOCHS_DIR, EPOCH),
        &EpochDoc {
            epoch: EPOCH,
            start_at: "2026-07-30T00:00:00Z".parse().expect("timestamp"),
            end_at: "2026-07-31T00:00:00Z".parse().expect("timestamp"),
            transaction_count: 0,
            supply: Decimal::ZERO,
            inflation: 0f64,
            inflation_taper: 0f64,
            slots_per_year: 0f64,
        },
    )
    .await
    .expect("seal epoch");

    store(
        &directory,
        "closed",
        &snapshot(EPOCH, Some("A"), Some(hetzner())),
    )
    .await;

    assert!(
        stored(&directory, EPOCH).await.is_none(),
        "the closed epoch's document must stay as close-epoch left it"
    );
}
