use crate::cache::Cache;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_postgres::Client;

pub struct Context {
    pub psql_client: Client,
    pub glossary_path: String,
    pub blacklist_path: String,
    pub scoring_url: String,
    pub cache: Cache,
}

impl Context {
    pub fn new(
        psql_client: Client,
        glossary_path: String,
        blacklist_path: String,
        scoring_url: String,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            psql_client,
            glossary_path,
            blacklist_path,
            scoring_url,
            cache: Cache::new(),
        })
    }
}

pub type WrappedContext = Arc<RwLock<Context>>;
