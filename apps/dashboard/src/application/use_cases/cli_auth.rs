//! CLI device-flow authorization.
//!
//! Three steps, mirroring OAuth 2.0 device authorization (RFC 8628) but
//! with the user *pasting the code back into the CLI* instead of the
//! CLI polling. The CLI sees neither cookie nor session JWT — it walks
//! away with a real developer API key (`sk_live_…`).
//!
//! 1. CLI → `POST /api/auth/cli/start` → `{ session_id, verification_uri }`.
//!    Stores `cli_auth:{session_id} = { status: pending, … }` for 10 min.
//! 2. Browser → `POST /api/auth/cli/authorize { session_id }`
//!    (CurrentUser-authenticated). Marks the session authorized, binds
//!    the user_id, generates a short verification code, stores its
//!    hash, returns the code for display. Raw API key is *not* yet
//!    issued — keeps secrets out of Redis.
//! 3. CLI → `POST /api/auth/cli/exchange { session_id, verification_code }`.
//!    Constant-time compare against the stored hash, mints the API key
//!    via `DeveloperAuthUseCases::issue_api_key_with_scopes`, deletes
//!    the session, returns `{ api_key, key_id, user_id, email }`.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    app_error::{AppError, AppResult},
    use_cases::{
        developer_auth::{
            DeveloperAuthUseCases, IssuedApiKey, NewScope, ScopeMatchType, ScopeResourceType,
        },
        user::UserRepo,
    },
};

const SESSION_TTL_SECS: i64 = 600; // 10 minutes
const VERIFICATION_CODE_LEN: usize = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliAuthSession {
    pub session_id: String,
    pub status: CliAuthStatus,
    pub client_name: String,
    pub hostname: String,
    pub started_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<Uuid>,
    /// SHA-256 hex of the displayed verification code. The plaintext
    /// is shown to the user once and never persisted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_code_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorized_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CliAuthStatus {
    Pending,
    Authorized,
}

#[async_trait]
pub trait CliAuthStore: Send + Sync {
    async fn put(&self, session: &CliAuthSession) -> AppResult<()>;
    async fn get(&self, session_id: &str) -> AppResult<Option<CliAuthSession>>;
    async fn delete(&self, session_id: &str) -> AppResult<()>;
}

#[derive(Debug, Serialize)]
pub struct CliAuthStartResponse {
    pub session_id: String,
    pub verification_uri: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct CliAuthAuthorizeResponse {
    pub verification_code: String,
    pub client_name: String,
    pub hostname: String,
}

#[derive(Debug, Serialize)]
pub struct CliAuthExchangeResponse {
    pub api_key: String,
    pub key_id: Uuid,
    pub user_id: Uuid,
    pub email: String,
}

pub struct CliAuthUseCases {
    store: Arc<dyn CliAuthStore>,
    developer_auth: Arc<DeveloperAuthUseCases>,
    user_repo: Arc<dyn UserRepo>,
    app_origin: String,
}

impl CliAuthUseCases {
    pub fn new(
        store: Arc<dyn CliAuthStore>,
        developer_auth: Arc<DeveloperAuthUseCases>,
        user_repo: Arc<dyn UserRepo>,
        app_origin: String,
    ) -> Self {
        Self {
            store,
            developer_auth,
            user_repo,
            app_origin,
        }
    }

    pub async fn start(
        &self,
        client_name: &str,
        hostname: &str,
    ) -> AppResult<CliAuthStartResponse> {
        let session_id = generate_session_id();
        let now = Utc::now();
        let expires_at = now + chrono::Duration::seconds(SESSION_TTL_SECS);
        let session = CliAuthSession {
            session_id: session_id.clone(),
            status: CliAuthStatus::Pending,
            client_name: clamp_text(client_name, 80),
            hostname: clamp_text(hostname, 80),
            started_at: now,
            expires_at,
            user_id: None,
            verification_code_hash: None,
            authorized_at: None,
        };
        self.store.put(&session).await?;
        let verification_uri = format!(
            "{}/cli-authorize?session={}",
            self.app_origin.trim_end_matches('/'),
            session_id
        );
        Ok(CliAuthStartResponse {
            session_id,
            verification_uri,
            expires_at,
        })
    }

    pub async fn authorize(
        &self,
        user_id: Uuid,
        session_id: &str,
    ) -> AppResult<CliAuthAuthorizeResponse> {
        let mut session = self
            .store
            .get(session_id)
            .await?
            .ok_or(AppError::NotFound)?;
        if session.status != CliAuthStatus::Pending {
            return Err(AppError::Conflict(
                "cli auth session already used or in wrong state".into(),
            ));
        }
        if session.expires_at <= Utc::now() {
            return Err(AppError::Conflict("cli auth session expired".into()));
        }

        let verification_code = generate_verification_code();
        let code_hash = sha256_hex(verification_code.as_bytes());
        session.status = CliAuthStatus::Authorized;
        session.user_id = Some(user_id);
        session.verification_code_hash = Some(code_hash);
        session.authorized_at = Some(Utc::now());
        self.store.put(&session).await?;

        Ok(CliAuthAuthorizeResponse {
            verification_code,
            client_name: session.client_name,
            hostname: session.hostname,
        })
    }

    pub async fn exchange(
        &self,
        session_id: &str,
        verification_code: &str,
    ) -> AppResult<CliAuthExchangeResponse> {
        let session = self
            .store
            .get(session_id)
            .await?
            .ok_or(AppError::NotFound)?;
        if session.status != CliAuthStatus::Authorized {
            return Err(AppError::Conflict(
                "cli auth session not yet authorized".into(),
            ));
        }
        if session.expires_at <= Utc::now() {
            return Err(AppError::Conflict("cli auth session expired".into()));
        }
        let expected_hash = session
            .verification_code_hash
            .as_deref()
            .ok_or_else(|| AppError::Internal("authorized session missing code hash".into()))?;
        let provided_hash = sha256_hex(verification_code.trim().as_bytes());
        if !constant_time_eq(expected_hash.as_bytes(), provided_hash.as_bytes()) {
            return Err(AppError::InvalidCredentials);
        }
        let user_id = session
            .user_id
            .ok_or_else(|| AppError::Internal("authorized session missing user_id".into()))?;

        // CLI keys mint with both data-plane (Bucket / All / rw) and
        // control-plane (Control / All / rw) scopes — the CLI needs
        // the latter to reach `/api/developer/*` for buckets, keys,
        // profile, billing. v2 will surface a scope picker on the
        // cli-authorize page so an operator can mint a read-only or
        // data-plane-only CLI key.
        let scopes = vec![
            NewScope {
                resource_type: ScopeResourceType::Bucket,
                match_type: ScopeMatchType::All,
                resource_value: None,
                can_read: true,
                can_write: true,
            },
            NewScope {
                resource_type: ScopeResourceType::Control,
                match_type: ScopeMatchType::All,
                resource_value: None,
                can_read: true,
                can_write: true,
            },
        ];

        // Include the first 6 chars of the session_id so back-to-back
        // `shardd auth login`s from the same host produce distinct,
        // identifiable rows in the dashboard's keys list. Without
        // this every CLI key on the same machine collides on
        // `cli/<hostname>` and the rows look like duplicates.
        let session_suffix = session_id.chars().take(6).collect::<String>();
        let key_name = format!("cli/{}/{}", session.hostname, session_suffix);
        let issued: IssuedApiKey = self
            .developer_auth
            .issue_api_key_with_scopes(user_id, &key_name, None, &scopes)
            .await?;

        // One-shot: invalidate the session immediately so the same
        // verification_code can't be exchanged twice.
        self.store.delete(session_id).await?;

        let profile = self
            .user_repo
            .get_profile_by_id(user_id)
            .await?
            .ok_or(AppError::InvalidCredentials)?;

        Ok(CliAuthExchangeResponse {
            api_key: issued.raw_key,
            key_id: issued.api_key.id,
            user_id,
            email: profile.email,
        })
    }
}

fn generate_session_id() -> String {
    let mut bytes = [0u8; 18]; // 144 bits → 24 base64-url chars
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn generate_verification_code() -> String {
    // Crockford-base32-flavoured alphabet minus visually ambiguous
    // 0/O/1/I/L. 31^10 ≈ 8.2e14 ≈ 49 bits — adequate for a 10-min,
    // rate-limited exchange window.
    const ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
    let mut bytes = [0u8; VERIFICATION_CODE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes
        .iter()
        .map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char)
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn clamp_text(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_code_is_alphabet_only() {
        const ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
        for _ in 0..100 {
            let code = generate_verification_code();
            assert_eq!(code.len(), VERIFICATION_CODE_LEN);
            for c in code.bytes() {
                assert!(ALPHABET.contains(&c), "unexpected char {c} in {code}");
            }
        }
    }

    #[test]
    fn constant_time_eq_handles_lengths() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"x"));
        assert!(constant_time_eq(b"", b""));
    }
}
