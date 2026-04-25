use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;
use sqlx::FromRow;
use uuid::Uuid;

use crate::{
    adapters::persistence::PostgresPersistence,
    app_error::{AppError, AppResult},
    use_cases::developer_auth::{
        ApiKeyWithOwner, DeveloperAccount, DeveloperApiKey, DeveloperApiKeyScope,
        DeveloperAuthAuditEntry, DeveloperAuthRepo, NewDeveloperAuthAuditEntry, NewScope,
        ScopeMatchType, ScopeResourceType,
    },
};

#[derive(Debug, FromRow)]
struct DeveloperAccountRow {
    id: Uuid,
    email: String,
    display_name: Option<String>,
    is_frozen: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<DeveloperAccountRow> for DeveloperAccount {
    fn from(value: DeveloperAccountRow) -> Self {
        Self {
            id: value.id,
            email: value.email,
            display_name: value.display_name,
            is_frozen: value.is_frozen,
            created_at: value.created_at,
            updated_at: value.updated_at,
        }
    }
}

#[derive(Debug, FromRow)]
struct DeveloperApiKeyRow {
    id: Uuid,
    user_id: Uuid,
    name: String,
    key_prefix: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
    expires_at: Option<DateTime<Utc>>,
}

impl From<DeveloperApiKeyRow> for DeveloperApiKey {
    fn from(value: DeveloperApiKeyRow) -> Self {
        Self {
            id: value.id,
            user_id: value.user_id,
            name: value.name,
            key_prefix: value.key_prefix,
            created_at: value.created_at,
            updated_at: value.updated_at,
            last_used_at: value.last_used_at,
            revoked_at: value.revoked_at,
            expires_at: value.expires_at,
        }
    }
}

#[derive(Debug, FromRow)]
struct DeveloperApiKeyScopeRow {
    id: Uuid,
    api_key_id: Uuid,
    resource_type: String,
    match_type: String,
    resource_value: Option<String>,
    can_read: bool,
    can_write: bool,
    created_at: DateTime<Utc>,
}

impl TryFrom<DeveloperApiKeyScopeRow> for DeveloperApiKeyScope {
    type Error = AppError;

    fn try_from(value: DeveloperApiKeyScopeRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: value.id,
            api_key_id: value.api_key_id,
            resource_type: parse_resource_type(&value.resource_type)?,
            match_type: parse_match_type(&value.match_type)?,
            resource_value: value.resource_value,
            can_read: value.can_read,
            can_write: value.can_write,
            created_at: value.created_at,
        })
    }
}

#[derive(Debug, FromRow)]
struct ApiKeyLookupRow {
    api_key_id: Uuid,
    user_id: Uuid,
    api_key_name: String,
    key_prefix: String,
    api_key_created_at: DateTime<Utc>,
    api_key_updated_at: DateTime<Utc>,
    api_key_last_used_at: Option<DateTime<Utc>>,
    api_key_revoked_at: Option<DateTime<Utc>>,
    api_key_expires_at: Option<DateTime<Utc>>,
    user_email: String,
    user_is_frozen: bool,
    user_created_at: DateTime<Utc>,
    user_updated_at: DateTime<Utc>,
}

impl From<ApiKeyLookupRow> for ApiKeyWithOwner {
    fn from(value: ApiKeyLookupRow) -> Self {
        Self {
            api_key: DeveloperApiKey {
                id: value.api_key_id,
                user_id: value.user_id,
                name: value.api_key_name,
                key_prefix: value.key_prefix,
                created_at: value.api_key_created_at,
                updated_at: value.api_key_updated_at,
                last_used_at: value.api_key_last_used_at,
                revoked_at: value.api_key_revoked_at,
                expires_at: value.api_key_expires_at,
            },
            user: DeveloperAccount {
                id: value.user_id,
                email: value.user_email,
                display_name: None,
                is_frozen: value.user_is_frozen,
                created_at: value.user_created_at,
                updated_at: value.user_updated_at,
            },
        }
    }
}

#[derive(Debug, FromRow)]
struct DeveloperAuthAuditRow {
    id: Uuid,
    admin_id: Option<Uuid>,
    admin_email: String,
    action: String,
    target_user_id: Option<Uuid>,
    api_key_id: Option<Uuid>,
    metadata: JsonValue,
    created_at: DateTime<Utc>,
}

impl From<DeveloperAuthAuditRow> for DeveloperAuthAuditEntry {
    fn from(value: DeveloperAuthAuditRow) -> Self {
        Self {
            id: value.id,
            admin_id: value.admin_id,
            admin_email: value.admin_email,
            action: value.action,
            target_user_id: value.target_user_id,
            api_key_id: value.api_key_id,
            metadata: value.metadata,
            created_at: value.created_at,
        }
    }
}

#[async_trait]
impl DeveloperAuthRepo for PostgresPersistence {
    async fn get_developer_account(&self, user_id: Uuid) -> AppResult<Option<DeveloperAccount>> {
        let row = sqlx::query_as::<_, DeveloperAccountRow>(
            r#"
                SELECT id, email, display_name, is_frozen, created_at, updated_at
                FROM users
                WHERE id = $1
            "#,
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AppError::from)?;
        Ok(row.map(Into::into))
    }

    async fn create_api_key_with_scopes(
        &self,
        user_id: Uuid,
        name: &str,
        key_prefix: &str,
        key_hash: &str,
        expires_at: Option<DateTime<Utc>>,
        scopes: &[NewScope],
    ) -> AppResult<DeveloperApiKey> {
        let mut tx = self.pool.begin().await.map_err(AppError::from)?;

        let key_row = sqlx::query_as::<_, DeveloperApiKeyRow>(
            r#"
                INSERT INTO developer_api_keys
                    (id, user_id, name, key_prefix, key_hash, expires_at)
                VALUES ($1, $2, $3, $4, $5, $6)
                RETURNING id, user_id, name, key_prefix, created_at, updated_at,
                          last_used_at, revoked_at, expires_at
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(user_id)
        .bind(name)
        .bind(key_prefix)
        .bind(key_hash)
        .bind(expires_at)
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::from)?;

        for scope in scopes {
            sqlx::query(
                r#"
                    INSERT INTO developer_api_key_scopes
                        (id, api_key_id, resource_type, match_type, resource_value, can_read, can_write)
                    VALUES ($1, $2, $3, $4, $5, $6, $7)
                "#,
            )
            .bind(Uuid::new_v4())
            .bind(key_row.id)
            .bind(scope.resource_type.to_string())
            .bind(scope.match_type.to_string())
            .bind(scope.resource_value.as_deref())
            .bind(scope.can_read)
            .bind(scope.can_write)
            .execute(&mut *tx)
            .await
            .map_err(AppError::from)?;
        }

        tx.commit().await.map_err(AppError::from)?;
        Ok(key_row.into())
    }

    async fn list_api_keys(&self, user_id: Uuid) -> AppResult<Vec<DeveloperApiKey>> {
        let rows = sqlx::query_as::<_, DeveloperApiKeyRow>(
            r#"
                SELECT id, user_id, name, key_prefix, created_at, updated_at,
                       last_used_at, revoked_at, expires_at
                FROM developer_api_keys
                WHERE user_id = $1
                ORDER BY created_at DESC
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AppError::from)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn get_api_key(&self, api_key_id: Uuid) -> AppResult<Option<DeveloperApiKey>> {
        let row = sqlx::query_as::<_, DeveloperApiKeyRow>(
            r#"
                SELECT id, user_id, name, key_prefix, created_at, updated_at,
                       last_used_at, revoked_at, expires_at
                FROM developer_api_keys
                WHERE id = $1
            "#,
        )
        .bind(api_key_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AppError::from)?;
        Ok(row.map(Into::into))
    }

    async fn lookup_api_key_by_hash(&self, key_hash: &str) -> AppResult<Option<ApiKeyWithOwner>> {
        let row = sqlx::query_as::<_, ApiKeyLookupRow>(
            r#"
                SELECT
                    k.id AS api_key_id,
                    k.user_id,
                    k.name AS api_key_name,
                    k.key_prefix,
                    k.created_at AS api_key_created_at,
                    k.updated_at AS api_key_updated_at,
                    k.last_used_at AS api_key_last_used_at,
                    k.revoked_at AS api_key_revoked_at,
                    k.expires_at AS api_key_expires_at,
                    u.email AS user_email,
                    u.is_frozen AS user_is_frozen,
                    u.created_at AS user_created_at,
                    u.updated_at AS user_updated_at
                FROM developer_api_keys k
                INNER JOIN users u ON u.id = k.user_id
                WHERE k.key_hash = $1
            "#,
        )
        .bind(key_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(AppError::from)?;
        Ok(row.map(Into::into))
    }

    async fn revoke_api_key(&self, api_key_id: Uuid) -> AppResult<()> {
        sqlx::query(
            r#"
                UPDATE developer_api_keys
                SET revoked_at = COALESCE(revoked_at, CURRENT_TIMESTAMP)
                WHERE id = $1
            "#,
        )
        .bind(api_key_id)
        .execute(&self.pool)
        .await
        .map_err(AppError::from)?;
        Ok(())
    }

    async fn touch_api_key_last_used(&self, api_key_id: Uuid) -> AppResult<()> {
        sqlx::query("UPDATE developer_api_keys SET last_used_at = CURRENT_TIMESTAMP WHERE id = $1")
            .bind(api_key_id)
            .execute(&self.pool)
            .await
            .map_err(AppError::from)?;
        Ok(())
    }

    async fn create_scope(
        &self,
        api_key_id: Uuid,
        resource_type: ScopeResourceType,
        match_type: ScopeMatchType,
        resource_value: Option<&str>,
        can_read: bool,
        can_write: bool,
    ) -> AppResult<DeveloperApiKeyScope> {
        let row = sqlx::query_as::<_, DeveloperApiKeyScopeRow>(
            r#"
                INSERT INTO developer_api_key_scopes
                    (id, api_key_id, resource_type, match_type, resource_value, can_read, can_write)
                VALUES ($1, $2, $3, $4, $5, $6, $7)
                RETURNING id, api_key_id, resource_type, match_type, resource_value,
                          can_read, can_write, created_at
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(api_key_id)
        .bind(resource_type.to_string())
        .bind(match_type.to_string())
        .bind(resource_value)
        .bind(can_read)
        .bind(can_write)
        .fetch_one(&self.pool)
        .await
        .map_err(AppError::from)?;
        row.try_into()
    }

    async fn list_scopes(&self, api_key_id: Uuid) -> AppResult<Vec<DeveloperApiKeyScope>> {
        let rows = sqlx::query_as::<_, DeveloperApiKeyScopeRow>(
            r#"
                SELECT id, api_key_id, resource_type, match_type, resource_value,
                       can_read, can_write, created_at
                FROM developer_api_key_scopes
                WHERE api_key_id = $1
                ORDER BY created_at DESC
            "#,
        )
        .bind(api_key_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AppError::from)?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn get_scope(&self, scope_id: Uuid) -> AppResult<Option<DeveloperApiKeyScope>> {
        let row = sqlx::query_as::<_, DeveloperApiKeyScopeRow>(
            r#"
                SELECT id, api_key_id, resource_type, match_type, resource_value,
                       can_read, can_write, created_at
                FROM developer_api_key_scopes
                WHERE id = $1
            "#,
        )
        .bind(scope_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AppError::from)?;
        row.map(TryInto::try_into).transpose()
    }

    async fn delete_scope(&self, scope_id: Uuid) -> AppResult<()> {
        sqlx::query("DELETE FROM developer_api_key_scopes WHERE id = $1")
            .bind(scope_id)
            .execute(&self.pool)
            .await
            .map_err(AppError::from)?;
        Ok(())
    }

    async fn log_developer_auth_audit(&self, entry: NewDeveloperAuthAuditEntry) -> AppResult<()> {
        sqlx::query(
            r#"
                INSERT INTO developer_auth_audit_log
                    (admin_id, admin_email, action, target_user_id, api_key_id, metadata)
                VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(entry.admin_id)
        .bind(entry.admin_email)
        .bind(entry.action)
        .bind(entry.target_user_id)
        .bind(entry.api_key_id)
        .bind(entry.metadata)
        .execute(&self.pool)
        .await
        .map_err(AppError::from)?;
        Ok(())
    }

    async fn list_developer_auth_audit(
        &self,
        limit: i64,
        offset: i64,
    ) -> AppResult<Vec<DeveloperAuthAuditEntry>> {
        let rows = sqlx::query_as::<_, DeveloperAuthAuditRow>(
            r#"
                SELECT id, admin_id, admin_email, action, target_user_id, api_key_id,
                       metadata, created_at
                FROM developer_auth_audit_log
                ORDER BY created_at DESC
                LIMIT $1 OFFSET $2
            "#,
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(AppError::from)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn count_developer_auth_audit(&self) -> AppResult<i64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM developer_auth_audit_log")
            .fetch_one(&self.pool)
            .await
            .map_err(AppError::from)?;
        Ok(count)
    }
}

fn parse_resource_type(value: &str) -> AppResult<ScopeResourceType> {
    match value {
        "bucket" => Ok(ScopeResourceType::Bucket),
        "control" => Ok(ScopeResourceType::Control),
        other => Err(AppError::Internal(format!(
            "unknown scope resource type in storage: {other}"
        ))),
    }
}

fn parse_match_type(value: &str) -> AppResult<ScopeMatchType> {
    match value {
        "all" => Ok(ScopeMatchType::All),
        "exact" => Ok(ScopeMatchType::Exact),
        "prefix" => Ok(ScopeMatchType::Prefix),
        other => Err(AppError::Internal(format!(
            "unknown scope match type in storage: {other}"
        ))),
    }
}
