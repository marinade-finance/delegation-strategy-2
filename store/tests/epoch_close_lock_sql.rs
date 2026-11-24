mod common;

use common::{empty_client, skip_without_database};
use std::time::Duration;
use store::utils::{lock_epoch_close, unlock_epoch_close};

#[tokio::test]
async fn a_waiting_close_gets_the_lock_once_the_store_releases_it() {
    let schema = "ds_test_epoch_close_lock_1";
    if skip_without_database(schema) {
        return;
    }
    let epoch = 900_001;
    let store = empty_client(schema).await.unwrap();
    let close = empty_client(&format!("{schema}_close")).await.unwrap();

    lock_epoch_close(&store, epoch, "10s").await.unwrap();
    let waiting =
        tokio::spawn(async move { lock_epoch_close(&close, epoch, "10s").await.map(|_| close) });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !waiting.is_finished(),
        "the close waits while the store holds the lock"
    );

    unlock_epoch_close(&store, epoch).await.unwrap();
    let close = waiting.await.unwrap().unwrap();
    unlock_epoch_close(&close, epoch).await.unwrap();
    drop_schemas(&close, schema).await;
}

#[tokio::test]
async fn a_waiter_gives_up_after_the_timeout_and_keeps_no_timeout() {
    let schema = "ds_test_epoch_close_lock_2";
    if skip_without_database(schema) {
        return;
    }
    let epoch = 900_002;
    let store = empty_client(schema).await.unwrap();
    let close = empty_client(&format!("{schema}_close")).await.unwrap();

    lock_epoch_close(&store, epoch, "10s").await.unwrap();
    assert!(
        lock_epoch_close(&close, epoch, "200ms").await.is_err(),
        "a held lock fails the waiter after the timeout"
    );
    let lock_timeout: String = close
        .query_one("SHOW lock_timeout", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(lock_timeout, "0", "the timeout does not outlive the wait");

    unlock_epoch_close(&store, epoch).await.unwrap();
    drop_schemas(&close, schema).await;
}

#[tokio::test]
async fn a_holder_whose_connection_ends_frees_the_lock() {
    let schema = "ds_test_epoch_close_lock_3";
    if skip_without_database(schema) {
        return;
    }
    let epoch = 900_003;
    let store = empty_client(schema).await.unwrap();
    let close = empty_client(&format!("{schema}_close")).await.unwrap();

    lock_epoch_close(&store, epoch, "10s").await.unwrap();
    drop(store);

    lock_epoch_close(&close, epoch, "5s")
        .await
        .expect("a crashed store does not block the close");
    unlock_epoch_close(&close, epoch).await.unwrap();
    drop_schemas(&close, schema).await;
}

async fn drop_schemas(client: &tokio_postgres::Client, schema: &str) {
    client
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {schema} CASCADE; DROP SCHEMA IF EXISTS {schema}_close CASCADE"
        ))
        .await
        .unwrap();
}
