use crate::cache::Cache;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_postgres::Client;

pub struct Context {
    pub psql_client: Client,
    pub glossary_path: String,
    pub cache: Cache,
}

impl Context {
    pub fn new(psql_client: Client, glossary_path: String) -> anyhow::Result<Self> {
        Ok(Self {
            psql_client,
            glossary_path,
            cache: Cache::new(),
        })
    }
}

pub type WrappedContext = Arc<RwLock<Context>>;
