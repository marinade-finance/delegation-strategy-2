use store::directory::{Fetch, Precondition};

mod common;

#[derive(Debug, PartialEq, serde::Deserialize, serde::Serialize)]
struct Doc {
    epoch: u64,
}

#[tokio::test]
async fn documents_round_trip_through_the_store() {
    let Some(store) = common::directory_store("round-trip").await else {
        return;
    };
    let directory = store.client();

    assert!(directory
        .get::<Doc>("/validators/snapshot/750")
        .await
        .expect("get")
        .is_none());

    let etag = directory
        .put(
            "/validators/snapshot/750",
            &Doc { epoch: 750 },
            Precondition::Create,
        )
        .await
        .expect("create");

    let doc = directory
        .get::<Doc>("/validators/snapshot/750")
        .await
        .expect("get")
        .expect("document");
    assert_eq!(doc.body, Doc { epoch: 750 });
    assert_eq!(doc.etag, etag);

    let fetch = directory
        .get_if_none_match::<Doc>("/validators/snapshot/750", &etag)
        .await
        .expect("conditional get");
    assert!(matches!(fetch, Fetch::NotModified));

    let created_again = directory
        .put(
            "/validators/snapshot/750",
            &Doc { epoch: 750 },
            Precondition::Create,
        )
        .await
        .expect_err("second create");
    assert!(created_again.is_conflict(), "{created_again}");

    let replaced = directory
        .put(
            "/validators/snapshot/750",
            &Doc { epoch: 751 },
            Precondition::IfMatch(etag.clone()),
        )
        .await
        .expect("replace");
    assert_ne!(replaced, etag);

    let stale = directory
        .put(
            "/validators/snapshot/750",
            &Doc { epoch: 752 },
            Precondition::IfMatch(etag),
        )
        .await
        .expect_err("stale write");
    assert!(stale.is_conflict(), "{stale}");

    for epoch in [9u64, 1000] {
        directory
            .put(
                &format!("/validators/snapshot/{epoch}"),
                &Doc { epoch },
                Precondition::Create,
            )
            .await
            .expect("create");
    }
    let names: Vec<String> = directory
        .list("/validators/snapshot")
        .await
        .expect("list")
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names, vec!["9", "750", "1000"]);

    assert!(directory
        .list("/validators/epochs")
        .await
        .expect("list of an unwritten parent")
        .is_empty());

    directory.ready().await.expect("ready");
}
