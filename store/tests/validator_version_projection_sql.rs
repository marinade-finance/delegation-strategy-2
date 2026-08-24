mod common;

use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use store::utils::{load_validators, ValidatorOverlays};

const EPOCH_OBSERVED: u64 = 996;
const EPOCH_BLANK: u64 = 997;
const EPOCH_OPEN: u64 = 998;

#[tokio::test]
async fn load_validators_projects_the_newest_observed_version() {
    let schema = "ds_test_load_validators_version_projection";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at)
             VALUES (1, $1, 0, NOW()), (1, $2, 0, NOW()), (1, $3, 0, NOW())",
            &[
                &Decimal::from(EPOCH_OBSERVED),
                &Decimal::from(EPOCH_BLANK),
                &Decimal::from(EPOCH_OPEN),
            ],
        )
        .await
        .unwrap();
    client
        .execute(
            "INSERT INTO validators (
                identity, vote_account, epoch, activated_stake, marinade_stake,
                marinade_native_stake, superminority, stake_to_become_superminority, credits,
                leader_slots, blocks_produced, skip_rate, updated_at, version, client_id,
                client_id_raw, feature_set, shred_version, rpc_public, pubsub_public
            ) VALUES
                ('identityGossipGap', 'voteGossipGap', $1, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), '4.1.0-rc.1', 3, 'Agave', 123, 456, false, false),
                ('identityGossipGap', 'voteGossipGap', $2, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), NULL, NULL, NULL, NULL, NULL, NULL, NULL),
                ('identityGossipGap', 'voteGossipGap', $3, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), NULL, NULL, NULL, NULL, NULL, NULL, NULL),
                ('identitySeen', 'voteSeen', $2, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), '4.0.3', 3, 'Agave', 111, 222, false, false),
                ('identitySeen', 'voteSeen', $3, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), '4.1.0', 1, 'Jito', 333, 444, true, false),
                ('identityPartial', 'votePartial', $2, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), '4.0.3', 3, 'Agave', 111, 222, false, false),
                ('identityPartial', 'votePartial', $3, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), NULL, 1, 'Jito', 333, 444, false, false),
                ('identityRetained', 'voteRetained', $2, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), '4.0.3', 3, 'Agave', 111, 222, false, false),
                ('identityRetained', 'voteRetained', $3, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), '4.1.0', 1, 'Jito', NULL, NULL, NULL, NULL),
                ('identityNeverSeen', 'voteNeverSeen', $3, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), NULL, NULL, NULL, NULL, NULL, NULL, NULL)",
            &[
                &Decimal::from(EPOCH_OBSERVED),
                &Decimal::from(EPOCH_BLANK),
                &Decimal::from(EPOCH_OPEN),
            ],
        )
        .await
        .unwrap();

    let validators = load_validators(
        &client,
        "http://127.0.0.1:1".to_string(),
        3,
        1,
        &ValidatorOverlays::default(),
    )
    .await
    .unwrap();

    let gap = validators
        .get("voteGossipGap")
        .expect("voteGossipGap must load");
    assert_eq!(
        gap.version,
        Some("4.1.0-rc.1".to_string()),
        "two versionless epochs must not read as a versionless validator"
    );
    assert_eq!(gap.client_id, Some(3));
    assert_eq!(gap.client_name, "Agave");
    assert_eq!(gap.client_label, "Agave");
    assert_eq!(gap.client_vendor, Some("agave".to_string()));
    assert_eq!(gap.client_lineage, Some("agave".to_string()));
    assert_eq!(gap.client_id_raw, Some("Agave".to_string()));
    assert_eq!(gap.feature_set, Some(123));
    assert_eq!(gap.shred_version, Some(456));
    assert_eq!(
        gap.epoch_stats
            .iter()
            .map(|s| (s.epoch, s.version.clone()))
            .collect::<Vec<_>>(),
        vec![
            (EPOCH_OPEN, None),
            (EPOCH_BLANK, None),
            (EPOCH_OBSERVED, Some("4.1.0-rc.1".to_string())),
        ],
        "the epochs that observed nothing must stay empty"
    );
    assert_eq!(gap.epoch_stats[0].client_id, None);
    assert_eq!(gap.epoch_stats[0].client_name, "Unknown");
    assert_eq!(gap.epoch_stats[0].feature_set, None);
    assert_eq!(gap.epoch_stats[0].shred_version, None);
    assert_eq!(gap.epoch_stats[2].client_id, Some(3));
    assert_eq!(gap.epoch_stats[2].client_name, "Agave");
    assert_eq!(gap.epoch_stats[2].feature_set, Some(123));
    assert_eq!(gap.epoch_stats[2].shred_version, Some(456));

    let seen = validators.get("voteSeen").unwrap();
    assert_eq!(
        seen.version,
        Some("4.1.0".to_string()),
        "an upgrade in the newest epoch must not be shadowed by the epoch below it"
    );
    assert_eq!(seen.client_id, Some(1));
    assert_eq!(seen.client_name, "Jito Labs");
    assert_eq!(seen.client_label, "Agave + Jito");
    assert_eq!(seen.client_vendor, Some("jito".to_string()));
    assert_eq!(seen.client_lineage, Some("agave".to_string()));
    assert_eq!(seen.client_id_raw, Some("Jito".to_string()));
    assert_eq!(seen.feature_set, Some(333));
    assert_eq!(seen.shred_version, Some(444));

    let partial = validators.get("votePartial").unwrap();
    assert_eq!(partial.version, None);
    assert_eq!(partial.client_id, Some(1));
    assert_eq!(partial.client_name, "Jito Labs");
    assert_eq!(partial.feature_set, Some(333));
    assert_eq!(partial.shred_version, Some(444));

    let retained = validators.get("voteRetained").unwrap();
    assert_eq!(retained.version, Some("4.1.0".to_string()));
    assert_eq!(retained.client_id, Some(1));
    assert_eq!(retained.client_name, "Jito Labs");
    assert_eq!(retained.feature_set, None);
    assert_eq!(retained.shred_version, None);
    assert_eq!(
        validators.get("voteNeverSeen").unwrap().version,
        None,
        "a validator never observed has nothing to project"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
