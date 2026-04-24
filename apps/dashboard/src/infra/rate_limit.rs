use redis::{AsyncCommands, aio::ConnectionManager};

use crate::app_error::{AppError, AppResult};

#[derive(Clone)]
pub struct RateLimiter {
    manager: ConnectionManager,
    window_secs: u64,
    max_per_ip: u64,
    max_per_email: u64,
}

impl RateLimiter {
    pub async fn new(
        redis_url: &str,
        window_secs: u64,
        max_per_ip: u64,
        max_per_email: u64,
    ) -> AppResult<Self> {
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
        Ok(Self {
            manager,
            window_secs,
            max_per_ip,
            max_per_email,
        })
    }

    pub async fn check(&self, ip: &str, email: Option<&str>) -> AppResult<()> {
        let mut conn = self.manager.clone();
        self.bump(&mut conn, &format!("rate:ip:{ip}"), self.max_per_ip)
            .await?;

        if let Some(email) = email {
            let normalized = email.to_lowercase();
            self.bump(
                &mut conn,
                &format!("rate:email:{normalized}"),
                self.max_per_email,
            )
            .await?;
        }
        Ok(())
    }

    async fn bump(&self, conn: &mut ConnectionManager, key: &str, limit: u64) -> AppResult<()> {
        let current: u64 = conn
            .incr(key, 1u32)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;

        if current == 1 {
            let _: () = conn
                .expire(key, self.window_secs as i64)
                .await
                .map_err(|e| AppError::Internal(e.to_string()))?;
        }

        if current > limit {
            return Err(AppError::RateLimited);
        }

        Ok(())
    }
}
