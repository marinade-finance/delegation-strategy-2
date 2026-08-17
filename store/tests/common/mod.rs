// Each tests/*.rs is its own crate and pulls in this whole module, so helpers only some of them need read as dead there.
#![allow(dead_code)]

use collect::validators::{Snapshot, ValidatorSnapshot};
use collect::validators_performance::ValidatorPerformance;
use store::validators::{store_validators, StoreValidatorsParams};
use structopt::StructOpt;
use tokio_postgres::{Client, NoTls};

pub const POSTGRES_URL_ENV: &str = "DS_TEST_POSTGRES_URL";

pub async fn migrated_client(schema: &str) -> Option<Client> {
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

pub fn skip_without_database(schema: &str) -> bool {
    if std::env::var(POSTGRES_URL_ENV).is_ok() {
        return false;
    }
    eprintln!("skipping {schema}: {POSTGRES_URL_ENV} is not set");
    true
}

pub fn write_yaml(name: &str, contents: &str) -> String {
    let path = std::env::temp_dir().join(format!("ds-test-{name}.yaml"));
    std::fs::write(&path, contents).unwrap();
    path.to_str().unwrap().to_string()
}

// One validator carrying only what the columns under test are read against; every caller overrides the field its own assertions turn on.
pub fn validator_snapshot(epoch: u64, identity: &str, vote_account: &str) -> Snapshot {
    Snapshot {
        epoch,
        created_at: "2026-07-31T00:00:00Z".into(),
        validators: vec![ValidatorSnapshot {
            identity: identity.into(),
            vote_account: vote_account.into(),
            node_ip: None,
            gossip_port: None,
            rpc_public: None,
            pubsub_public: None,
            info_name: None,
            info_url: None,
            info_details: None,
            info_keybase: None,
            info_icon_url: None,
            data_center: None,
            activated_stake: 100,
            foundation_stake: 0,
            self_stake: 0,
            marinade_stake: 0,
            marinade_native_stake: 0,
            institutional_stake: 0,
            superminority: false,
            stake_to_become_superminority: 0,
            performance: ValidatorPerformance {
                commission: 7,
                version: Some("2.0.0".into()),
                client_id: None,
                client_id_raw: None,
                feature_set: None,
                shred_version: None,
                credits: 10,
                leader_slots: 100,
                blocks_produced: 100,
                skip_rate: 0f64,
                delinquent: false,
            },
        }],
    }
}

pub async fn store_snapshot(client: &mut Client, name: &str, snapshot: &Snapshot) {
    let path = write_yaml(name, &serde_yaml::to_string(snapshot).unwrap());
    store_validators(
        StoreValidatorsParams::from_iter(["store", "--snapshot-file", &path]),
        client,
    )
    .await
    .unwrap();
    std::fs::remove_file(path).unwrap();
}
