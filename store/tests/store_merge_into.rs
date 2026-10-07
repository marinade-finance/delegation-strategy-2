use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use store::directory::Directory;
use store::docs::{merge_into, put_whole};

mod common;

const PATH: &str = "/validators/snapshot/1000";

/// A document whose merge rule is addition of keys, plus a write to stage
/// against it. `race` never reaches the store: it rides on the incoming value
/// so that `merge`, which takes no other argument, can reach it.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
struct Counts {
    counts: BTreeMap<String, u64>,
    #[serde(skip)]
    race: Option<Race>,
}

impl Counts {
    fn of(key: &str, count: u64) -> Self {
        Self {
            counts: BTreeMap::from([(key.to_string(), count)]),
            race: None,
        }
    }

    fn racing(mut self, race: Race) -> Self {
        self.race = Some(race);
        self
    }
}

/// Another client's whole-document write, run from inside `merge` — the one
/// point where `merge_into` holds a precondition it has not written against
/// yet, so the race needs no sleep to be the race it claims to be.
#[derive(Debug, Clone)]
struct Race {
    url: String,
    token: String,
    counts: BTreeMap<String, u64>,
    done: Arc<AtomicBool>,
}

impl Race {
    fn new(store: &common::DirectoryStore, key: &str, count: u64) -> Self {
        Self {
            url: store.url.clone(),
            token: store.token.clone(),
            counts: BTreeMap::from([(key.to_string(), count)]),
            done: Default::default(),
        }
    }

    /// Once only: `merge_into` merges again when it retries, and a second
    /// write would be a race against the retry rather than the first attempt.
    fn run(&self) {
        if self.done.swap(true, Ordering::SeqCst) {
            return;
        }
        std::thread::scope(|scope| {
            scope.spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("racing runtime")
                    .block_on(self.write());
            });
        });
    }

    async fn write(&self) {
        let directory =
            Directory::new(self.url.clone(), self.token.clone()).expect("racing client");
        let body = Counts {
            counts: self.counts.clone(),
            race: None,
        };
        put_whole(&directory, PATH, &body)
            .await
            .expect("racing write");
    }
}

fn merge_counts(doc: &mut Counts, incoming: Counts) {
    if let Some(race) = &incoming.race {
        race.run();
    }
    doc.counts.extend(incoming.counts);
}

/// A first write that loses the create must fold into what landed rather than
/// replace it, which is the whole point of the one retry.
#[tokio::test]
async fn merge_into_folds_into_a_write_that_beat_it_to_the_create() {
    let Some(store) = common::directory_store("merge-into-create").await else {
        return;
    };
    let directory = store.client();

    merge_into(
        &directory,
        PATH,
        Counts::of("a", 1).racing(Race::new(&store, "b", 2)),
        merge_counts,
    )
    .await
    .expect("merge into a path another writer created first");

    let stored = directory
        .get::<Counts>(PATH)
        .await
        .expect("get")
        .expect("document");
    assert_eq!(
        stored.body.counts,
        BTreeMap::from([("a".to_string(), 1), ("b".to_string(), 2)])
    );
}

/// A lost `If-Match` race is not a create that can be folded, so it surfaces
/// instead of being retried over the top of the winner.
#[tokio::test]
async fn merge_into_surfaces_a_lost_if_match_race() {
    let Some(store) = common::directory_store("merge-into-conflict").await else {
        return;
    };
    let directory = store.client();

    put_whole(&directory, PATH, &Counts::of("b", 2))
        .await
        .expect("seed the document");

    let error = merge_into(
        &directory,
        PATH,
        Counts::of("a", 1).racing(Race::new(&store, "c", 3)),
        merge_counts,
    )
    .await
    .expect_err("a lost race");
    assert!(format!("{error}").contains("Conflict on"), "{error}");

    let stored = directory
        .get::<Counts>(PATH)
        .await
        .expect("get")
        .expect("document");
    assert_eq!(
        stored.body.counts,
        BTreeMap::from([("c".to_string(), 3)]),
        "the merge that lost the race must not have landed"
    );
}
