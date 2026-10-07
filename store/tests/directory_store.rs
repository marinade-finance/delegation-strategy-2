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

    assert!(directory
        .resolve("/validators/epochs/@last")
        .await
        .expect("resolve of an empty collection")
        .is_none());

    let forbidden = directory
        .get::<Doc>("/other/path")
        .await
        .expect_err("a path outside the token's grants");
    assert!(forbidden.to_string().contains("403"), "{forbidden}");

    directory.ready().await.expect("ready");
}

/// The store answers a listing 100 rows at a time and names the next page in
/// `Link: rel="next"`; a parent with more children than one page must still
/// come back whole.
#[tokio::test]
async fn a_listing_longer_than_one_page_comes_back_whole() {
    let Some(store) = common::directory_store("long-listing").await else {
        return;
    };
    let directory = store.client();

    let epochs: Vec<u64> = (900..1050).collect();
    for epoch in epochs.iter().copied() {
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
    let expected: Vec<String> = epochs.iter().map(u64::to_string).collect();
    assert_eq!(names, expected);

    let last = directory
        .resolve("/validators/snapshot/@last")
        .await
        .expect("resolve")
        .expect("a resolved path");
    assert_eq!(last, "/validators/snapshot/1049");
}
