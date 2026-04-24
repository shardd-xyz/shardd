use async_trait::async_trait;
use redis::{AsyncCommands, aio::ConnectionManager};
use uuid::Uuid;

use crate::{
    app_error::{AppError, AppResult},
    use_cases::user::MagicLinkStore as MagicLinkStoreTrait,
};

#[derive(Clone)]
pub struct MagicLinkStore {
    manager: ConnectionManager,
}

impl MagicLinkStore {
    pub async fn new(redis_url: &str) -> AppResult<Self> {
        let client = redis::Client::open(redis_url).map_err(|e| {
            AppError::Internal(format!(
                "Redis connection failed (check redis password/URL): {e}"
            ))
        })?;
        let manager = ConnectionManager::new(client).await.map_err(|e| {
            AppError::Internal(format!(
                "Redis auth/connection failed (check redis password/URL): {e}"
            ))
        })?;

        Ok(Self { manager })
    }

    fn key(token_hash: &str) -> String {
        format!("magic:{token_hash}")
    }
}

#[async_trait]
impl MagicLinkStoreTrait for MagicLinkStore {
    async fn save(&self, token_hash: &str, user_id: Uuid, ttl_minutes: i64) -> AppResult<()> {
        let mut conn = self.manager.clone();
        let key = Self::key(token_hash);
        let ttl_secs: u64 = (ttl_minutes.max(1) * 60) as u64;

        let _: () = conn
            .set_ex(key, user_id.to_string(), ttl_secs)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;

        Ok(())
    }

    async fn consume(&self, token_hash: &str) -> AppResult<Option<Uuid>> {
        let mut conn = self.manager.clone();
        let key = Self::key(token_hash);

        let raw: Option<String> = redis::cmd("GETDEL")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;

        raw.map(|value| Uuid::parse_str(&value).map_err(|e| AppError::Internal(e.to_string())))
            .transpose()
    }
}
