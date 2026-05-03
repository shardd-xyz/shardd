use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::app_error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScopeResourceType {
    /// Data plane: read/write events in a bucket. Validated by the
    /// gateway via `/api/machine/introspect` against the
    /// (match_type, bucket) pair.
    Bucket,
    /// Control plane: dashboard-equivalent reach for one user
    /// (manage their own buckets, keys, profile, billing). Required
    /// to call `/api/developer/*` and read-only `/api/user/*` with
    /// an API key. `match_type=All`, `resource_value=NULL` are the
    /// only valid combinations today; can_read/can_write distinguish
    /// browse-only keys from full-control keys.
    Control,
}

impl std::fmt::Display for ScopeResourceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bucket => write!(f, "bucket"),
            Self::Control => write!(f, "control"),
        }
    }
}

/// Predicate: does this key carry at least one active control-plane
/// scope? Used by `Authenticated` to gate API-key access to
/// `/api/developer/*` so that older keys (Bucket scopes only) keep
/// their data-plane reach but lose control-plane access. Read-only
/// for now — `can_read` alone is enough to GET; mutating endpoints
/// also require `can_write`. The current Authenticated impl only
/// gates on presence; per-route read/write split is a follow-up.
pub fn key_has_control_scope(scopes: &[DeveloperApiKeyScope]) -> bool {
    scopes
        .iter()
        .any(|s| s.resource_type == ScopeResourceType::Control && (s.can_read || s.can_write))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScopeMatchType {
    All,
    Exact,
    Prefix,
}

impl std::fmt::Display for ScopeMatchType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::All => write!(f, "all"),
            Self::Exact => write!(f, "exact"),
            Self::Prefix => write!(f, "prefix"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MachineAction {
    Read,
    Write,
    ReadOwnAccount,
    WriteOwnAccount,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeveloperAccount {
    pub id: Uuid,
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub is_frozen: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeveloperApiKey {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub key_prefix: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeveloperApiKeyScope {
    pub id: Uuid,
    pub api_key_id: Uuid,
    pub resource_type: ScopeResourceType,
    pub match_type: ScopeMatchType,
    pub resource_value: Option<String>,
    pub can_read: bool,
    pub can_write: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ApiKeyWithOwner {
    pub api_key: DeveloperApiKey,
    pub user: DeveloperAccount,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeveloperAuthAuditEntry {
    pub id: Uuid,
    pub admin_id: Option<Uuid>,
    pub admin_email: String,
    pub action: String,
    pub target_user_id: Option<Uuid>,
    pub api_key_id: Option<Uuid>,
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewDeveloperAuthAuditEntry {
    pub admin_id: Uuid,
    pub admin_email: String,
    pub action: String,
    pub target_user_id: Option<Uuid>,
    pub api_key_id: Option<Uuid>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct IssuedApiKey {
    pub api_key: DeveloperApiKey,
    pub raw_key: String,
}

/// Scope input for atomic `create_api_key_with_scopes`. Pre-validated by the
/// route handler before reaching the use-case.
#[derive(Debug, Clone)]
pub struct NewScope {
    pub resource_type: ScopeResourceType,
    pub match_type: ScopeMatchType,
    pub resource_value: Option<String>,
    pub can_read: bool,
    pub can_write: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MatchedScope {
    pub match_type: ScopeMatchType,
    pub resource_value: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MachineAuthDecision {
    pub valid: bool,
    pub allowed: bool,
    pub user_id: Option<Uuid>,
    pub cache_ttl_ms: u64,
    pub denial_reason: Option<String>,
    pub matched_scope: Option<MatchedScope>,
}

#[async_trait]
pub trait DeveloperAuthRepo: Send + Sync {
    async fn get_developer_account(&self, user_id: Uuid) -> AppResult<Option<DeveloperAccount>>;
    /// Atomic: insert the key row and all supplied scope rows in one
    /// transaction. If any insert fails, the whole thing rolls back.
    async fn create_api_key_with_scopes(
        &self,
        user_id: Uuid,
        name: &str,
        key_prefix: &str,
        key_hash: &str,
        expires_at: Option<DateTime<Utc>>,
        scopes: &[NewScope],
    ) -> AppResult<DeveloperApiKey>;
    async fn list_api_keys(&self, user_id: Uuid) -> AppResult<Vec<DeveloperApiKey>>;
    async fn get_api_key(&self, api_key_id: Uuid) -> AppResult<Option<DeveloperApiKey>>;
    async fn lookup_api_key_by_hash(&self, key_hash: &str) -> AppResult<Option<ApiKeyWithOwner>>;
    async fn revoke_api_key(&self, api_key_id: Uuid) -> AppResult<()>;
    async fn touch_api_key_last_used(&self, api_key_id: Uuid) -> AppResult<()>;
    async fn create_scope(
        &self,
        api_key_id: Uuid,
        resource_type: ScopeResourceType,
        match_type: ScopeMatchType,
        resource_value: Option<&str>,
        can_read: bool,
        can_write: bool,
    ) -> AppResult<DeveloperApiKeyScope>;
    async fn list_scopes(&self, api_key_id: Uuid) -> AppResult<Vec<DeveloperApiKeyScope>>;
    async fn get_scope(&self, scope_id: Uuid) -> AppResult<Option<DeveloperApiKeyScope>>;
    async fn delete_scope(&self, scope_id: Uuid) -> AppResult<()>;
    async fn log_developer_auth_audit(&self, entry: NewDeveloperAuthAuditEntry) -> AppResult<()>;
    async fn list_developer_auth_audit(
        &self,
        limit: i64,
        offset: i64,
    ) -> AppResult<Vec<DeveloperAuthAuditEntry>>;
    async fn count_developer_auth_audit(&self) -> AppResult<i64>;
}

#[derive(Clone)]
pub struct DeveloperAuthUseCases {
    repo: Arc<dyn DeveloperAuthRepo>,
    positive_cache_ttl_ms: u64,
}

impl DeveloperAuthUseCases {
    pub fn new(repo: Arc<dyn DeveloperAuthRepo>, positive_cache_ttl_ms: u64) -> Self {
        Self {
            repo,
            positive_cache_ttl_ms,
        }
    }

    pub async fn get_account(&self, user_id: Uuid) -> AppResult<DeveloperAccount> {
        self.repo
            .get_developer_account(user_id)
            .await?
            .ok_or(AppError::NotFound)
    }
    pub async fn issue_api_key(
        &self,
        user_id: Uuid,
        name: &str,
        expires_at: Option<DateTime<Utc>>,
    ) -> AppResult<IssuedApiKey> {
        self.issue_api_key_with_scopes(user_id, name, expires_at, &[])
            .await
    }

    pub async fn issue_api_key_with_scopes(
        &self,
        user_id: Uuid,
        name: &str,
        expires_at: Option<DateTime<Utc>>,
        scopes: &[NewScope],
    ) -> AppResult<IssuedApiKey> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(AppError::InvalidInput("API key name is required".into()));
        }

        let raw_key = generate_api_key();
        let key_prefix = key_prefix(&raw_key);
        let key_hash = hash_api_key(&raw_key);
        let api_key = self
            .repo
            .create_api_key_with_scopes(
                user_id,
                trimmed,
                &key_prefix,
                &key_hash,
                expires_at,
                scopes,
            )
            .await?;
        Ok(IssuedApiKey { api_key, raw_key })
    }

    pub async fn rotate_api_key(&self, api_key_id: Uuid) -> AppResult<IssuedApiKey> {
        let existing = self
            .repo
            .get_api_key(api_key_id)
            .await?
            .ok_or(AppError::NotFound)?;
        self.repo.revoke_api_key(api_key_id).await?;
        self.issue_api_key(existing.user_id, &existing.name, existing.expires_at)
            .await
    }

    pub async fn introspect(
        &self,
        raw_key: &str,
        action: MachineAction,
        bucket: &str,
    ) -> AppResult<MachineAuthDecision> {
        if raw_key.trim().is_empty() {
            return Ok(MachineAuthDecision {
                valid: false,
                allowed: false,
                user_id: None,
                cache_ttl_ms: 0,
                denial_reason: Some("missing_api_key".into()),
                matched_scope: None,
            });
        }

        let key_hash = hash_api_key(raw_key);
        let Some(found) = self.repo.lookup_api_key_by_hash(&key_hash).await? else {
            return Ok(MachineAuthDecision {
                valid: false,
                allowed: false,
                user_id: None,
                cache_ttl_ms: 0,
                denial_reason: Some("invalid_api_key".into()),
                matched_scope: None,
            });
        };

        if found.user.is_frozen {
            return Ok(MachineAuthDecision {
                valid: false,
                allowed: false,
                user_id: Some(found.user.id),
                cache_ttl_ms: 0,
                denial_reason: Some("user_frozen".into()),
                matched_scope: None,
            });
        }
        if found.api_key.revoked_at.is_some() {
            return Ok(MachineAuthDecision {
                valid: false,
                allowed: false,
                user_id: Some(found.user.id),
                cache_ttl_ms: 0,
                denial_reason: Some("api_key_revoked".into()),
                matched_scope: None,
            });
        }
        if let Some(expires_at) = found.api_key.expires_at
            && expires_at <= Utc::now()
        {
            return Ok(MachineAuthDecision {
                valid: false,
                allowed: false,
                user_id: Some(found.user.id),
                cache_ttl_ms: 0,
                denial_reason: Some("api_key_expired".into()),
                matched_scope: None,
            });
        }

        let scopes = self.repo.list_scopes(found.api_key.id).await?;
        let matched_scope = scopes
            .into_iter()
            .find(|scope| scope_allows(scope, &action, bucket))
            .map(|scope| MatchedScope {
                match_type: scope.match_type,
                resource_value: scope.resource_value,
            });

        if matched_scope.is_none() {
            return Ok(MachineAuthDecision {
                valid: true,
                allowed: false,
                user_id: Some(found.user.id),
                cache_ttl_ms: 0,
                denial_reason: Some("scope_denied".into()),
                matched_scope: None,
            });
        }

        self.repo.touch_api_key_last_used(found.api_key.id).await?;

        Ok(MachineAuthDecision {
            valid: true,
            allowed: true,
            user_id: Some(found.user.id),
            cache_ttl_ms: self.positive_cache_ttl_ms,
            denial_reason: None,
            matched_scope,
        })
    }
}

pub fn generate_api_key() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!(
        "sk_live_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

pub fn key_prefix(raw_key: &str) -> String {
    raw_key.chars().take(16).collect()
}

pub fn hash_api_key(raw_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw_key.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn scope_allows(scope: &DeveloperApiKeyScope, action: &MachineAction, bucket: &str) -> bool {
    let permission_granted = match action {
        MachineAction::Read | MachineAction::ReadOwnAccount => scope.can_read,
        MachineAction::Write | MachineAction::WriteOwnAccount => scope.can_write,
    };
    if !permission_granted {
        return false;
    }

    if matches!(
        action,
        MachineAction::ReadOwnAccount | MachineAction::WriteOwnAccount
    ) {
        return scope.resource_type == ScopeResourceType::Control
            && scope.match_type == ScopeMatchType::All
            && scope.resource_value.is_none();
    }

    if scope.resource_type != ScopeResourceType::Bucket {
        return false;
    }

    match scope.match_type {
        ScopeMatchType::All => true,
        ScopeMatchType::Exact => scope.resource_value.as_deref() == Some(bucket),
        ScopeMatchType::Prefix => scope
            .resource_value
            .as_deref()
            .is_some_and(|value| bucket.starts_with(value)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(
        match_type: ScopeMatchType,
        value: Option<&str>,
        can_read: bool,
        can_write: bool,
    ) -> DeveloperApiKeyScope {
        DeveloperApiKeyScope {
            id: Uuid::new_v4(),
            api_key_id: Uuid::new_v4(),
            resource_type: ScopeResourceType::Bucket,
            match_type,
            resource_value: value.map(str::to_string),
            can_read,
            can_write,
            created_at: Utc::now(),
        }
    }

    fn control_scope(can_read: bool, can_write: bool) -> DeveloperApiKeyScope {
        DeveloperApiKeyScope {
            id: Uuid::new_v4(),
            api_key_id: Uuid::new_v4(),
            resource_type: ScopeResourceType::Control,
            match_type: ScopeMatchType::All,
            resource_value: None,
            can_read,
            can_write,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn prefix_scope_matches_subtree() {
        let scope = scope(ScopeMatchType::Prefix, Some("orders/"), true, false);
        assert!(scope_allows(&scope, &MachineAction::Read, "orders/eu"));
        assert!(!scope_allows(&scope, &MachineAction::Read, "payments/eu"));
    }

    #[test]
    fn exact_scope_requires_exact_bucket() {
        let scope = scope(ScopeMatchType::Exact, Some("orders"), false, true);
        assert!(scope_allows(&scope, &MachineAction::Write, "orders"));
        assert!(!scope_allows(&scope, &MachineAction::Write, "orders/eu"));
    }

    #[test]
    fn control_scope_allows_own_account_actions_only() {
        let read_only = control_scope(true, false);
        assert!(scope_allows(
            &read_only,
            &MachineAction::ReadOwnAccount,
            "*"
        ));
        assert!(!scope_allows(
            &read_only,
            &MachineAction::WriteOwnAccount,
            "*"
        ));
        assert!(!scope_allows(&read_only, &MachineAction::Read, "orders"));

        let bucket_scope = scope(ScopeMatchType::All, None, true, true);
        assert!(!scope_allows(
            &bucket_scope,
            &MachineAction::ReadOwnAccount,
            "*"
        ));
    }

    #[test]
    fn api_keys_are_prefixed_and_hashable() {
        let key = generate_api_key();
        assert!(key.starts_with("sk_live_"));
        assert!(key_prefix(&key).len() <= 16);
        assert_eq!(hash_api_key(&key).len(), 64);
    }
}
