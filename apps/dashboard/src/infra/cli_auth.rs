//! Redis-backed CliAuthStore. Mirrors the magic_links pattern — a thin
//! ConnectionManager wrapper that serialises CliAuthSession as JSON
//! under the `cli_auth:{session_id}` key with a TTL aligned to the
//! session's `expires_at`.

use async_trait::async_trait;
use redis::{AsyncCommands, aio::ConnectionManager};

use crate::{
    app_error::{AppError, AppResult},
    use_cases::cli_auth::{CliAuthSession, CliAuthStore as CliAuthStoreTrait},
};

#[derive(Clone)]
pub struct CliAuthStore {
    manager: ConnectionManager,
}

impl CliAuthStore {
    pub async fn new(redis_url: &str) -> AppResult<Self> {
        let client = redis::Client::open(redis_url)
            .map_err(|e| AppError::Internal(format!("Redis connection failed: {e}")))?;
        let manager = ConnectionManager::new(client)
            .await
            .map_err(|e| AppError::Internal(format!("Redis connection failed: {e}")))?;
        Ok(Self { manager })
    }

    fn key(session_id: &str) -> String {
        format!("cli_auth:{session_id}")
    }
}

#[async_trait]
impl CliAuthStoreTrait for CliAuthStore {
    async fn put(&self, session: &CliAuthSession) -> AppResult<()> {
        let mut conn = self.manager.clone();
        let key = Self::key(&session.session_id);
        let value = serde_json::to_string(session)
            .map_err(|e| AppError::Internal(format!("serialize cli_auth session: {e}")))?;
        let ttl_secs = (session.expires_at - chrono::Utc::now())
            .num_seconds()
            .max(1) as u64;
        let _: () = conn
            .set_ex(key, value, ttl_secs)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get(&self, session_id: &str) -> AppResult<Option<CliAuthSession>> {
        let mut conn = self.manager.clone();
        let key = Self::key(session_id);
        let raw: Option<String> = conn
            .get(key)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        match raw {
            None => Ok(None),
            Some(s) => Ok(Some(serde_json::from_str(&s).map_err(|e| {
                AppError::Internal(format!("decode cli_auth session: {e}"))
            })?)),
        }
    }

    async fn delete(&self, session_id: &str) -> AppResult<()> {
        let mut conn = self.manager.clone();
        let key = Self::key(session_id);
        let _: () = conn
            .del(key)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        Ok(())
    }
}
