use crate::cache::Cache;
use log::{error, info};
use std::{collections::HashMap, sync::Arc};
use store::dto::ValidatorRecord;
use tokio::{
    sync::RwLock,
    time::{sleep, Duration},
};
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

pub fn spawn_context_updater(context: WrappedContext) {
    tokio::spawn(async move {
        loop {
            info!("Updating the context");

            info!("Updating the cache");
            let validators =
                store::utils::load_validators(&context.read().await.psql_client, 10).await;

            info!("Len: {}", context.read().await.cache.validators.len());

            match validators {
                Ok(validators) => {
                    context
                        .write()
                        .await
                        .cache
                        .validators
                        .clone_from(&validators);
                }
                Err(err) => {
                    error!("Failed to update the validators list: {}", err);
                }
            };

            info!("Len: {}", context.read().await.cache.validators.len());

            sleep(Duration::from_secs(600)).await;
        }
    });
}
