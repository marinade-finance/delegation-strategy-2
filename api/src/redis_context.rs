use std::sync::Arc;
use tokio::sync::RwLock;

pub struct RedisContext {
    pub redis_client: redis::Client,
}

impl RedisContext {
    pub fn new(redis_client: redis::Client) -> anyhow::Result<Self> {
        Ok(Self { redis_client })
    }
}

pub type WrappedRedisContext = Arc<RwLock<RedisContext>>;
