use crate::cache::Cache;
use std::sync::Arc;
use store::directory::Directory;
use store::warehouse::Warehouse;
use tokio::sync::RwLock;

pub struct Context {
    pub directory: Directory,
    /// The documents every warm step folds over. Locked on its own, and never
    /// while the context's own lock is held, so a refresh cannot block a reader.
    pub warehouse: Arc<RwLock<Warehouse>>,
    pub glossary_path: String,
    pub blacklist_path: String,
    pub validator_bonds_api_url: String,
    pub apy_api_url: String,
    pub cache: Cache,
}

impl Context {
    pub fn new(
        directory: Directory,
        glossary_path: String,
        blacklist_path: String,
        validator_bonds_api_url: String,
        apy_api_url: String,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            directory,
            warehouse: Default::default(),
            glossary_path,
            blacklist_path,
            validator_bonds_api_url,
            apy_api_url,
            cache: Cache::new(),
        })
    }
}

pub type WrappedContext = Arc<RwLock<Context>>;
