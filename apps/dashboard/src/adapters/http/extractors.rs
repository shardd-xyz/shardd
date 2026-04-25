use axum::{extract::FromRequestParts, http::request::Parts};
use axum_extra::extract::cookie::CookieJar;
use chrono::Utc;
use uuid::Uuid;

use crate::{
    adapters::http::app_state::AppState,
    app_error::AppError,
    application::jwt,
    use_cases::{developer_auth::hash_api_key, user::UserProfile},
};

/// Extracts the current authenticated user from the access_token cookie.
/// Rejects frozen users with 403.
pub struct CurrentUser(pub UserProfile);

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        let access = jar
            .get("access_token")
            .ok_or(AppError::InvalidCredentials)?;
        let claims = jwt::verify(access.value(), &state.config.jwt_secret)?;
        let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::InvalidCredentials)?;
        let profile = state
            .user_repo
            .get_profile_by_id(user_id)
            .await?
            .ok_or(AppError::InvalidCredentials)?;
        if profile.is_frozen {
            return Err(AppError::AccountFrozen);
        }
        Ok(CurrentUser(profile))
    }
}

/// Extracts an authenticated user from either:
/// 1. `Authorization: Bearer sk_live_…` — a developer API key (used by
///    the customer CLI and any external programmatic caller). The key
///    is hashed and looked up in the developer_api_keys table; revoked
///    or expired keys are rejected. No scope check — any active key
///    grants the same dashboard-control reach as the user's session.
/// 2. The `access_token` cookie — the dashboard UI's session JWT.
///
/// Used on every `/api/developer/*` and read-only `/api/user` route so
/// the same handlers serve both the browser and the CLI. Sensitive
/// routes that still require a fresh session (account deletion, all
/// admin-only routes) keep the cookie-only `CurrentUser` extractor.
pub struct Authenticated(pub UserProfile);

impl FromRequestParts<AppState> for Authenticated {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if let Some(profile) = profile_from_bearer(parts, state).await? {
            return Ok(Authenticated(profile));
        }
        // Fall through to the cookie path. Reuses CurrentUser so any
        // future change to session validation lives in exactly one
        // place.
        let CurrentUser(profile) = CurrentUser::from_request_parts(parts, state).await?;
        Ok(Authenticated(profile))
    }
}

/// Looks for an `Authorization: Bearer …` header and, if present and
/// it parses as a developer API key, returns the owning user's profile.
/// Returns `Ok(None)` when no Bearer header is present (the caller falls
/// back to the cookie path); returns `Err` when a header *is* present
/// but the key is invalid/revoked/expired/frozen — i.e. the caller did
/// declare a key, just a bad one, so we don't silently fall through to
/// the cookie path with an attacker-controlled token in the header.
async fn profile_from_bearer(
    parts: &Parts,
    state: &AppState,
) -> Result<Option<UserProfile>, AppError> {
    let Some(header) = parts.headers.get(axum::http::header::AUTHORIZATION) else {
        return Ok(None);
    };
    let Ok(value) = header.to_str() else {
        return Err(AppError::InvalidCredentials);
    };
    let Some(raw_key) = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
    else {
        // A non-Bearer Authorization header (e.g. Basic) — let the
        // cookie path decide. Don't reject outright.
        return Ok(None);
    };
    let raw_key = raw_key.trim();
    if raw_key.is_empty() {
        return Err(AppError::InvalidCredentials);
    }

    let key_hash = hash_api_key(raw_key);
    let found = state
        .developer_auth_repo
        .lookup_api_key_by_hash(&key_hash)
        .await?
        .ok_or(AppError::InvalidCredentials)?;

    if found.api_key.revoked_at.is_some() {
        return Err(AppError::InvalidCredentials);
    }
    if let Some(expires_at) = found.api_key.expires_at
        && expires_at <= Utc::now()
    {
        return Err(AppError::InvalidCredentials);
    }
    if found.user.is_frozen {
        return Err(AppError::AccountFrozen);
    }

    // Load the full UserProfile so the extractor's return type matches
    // the cookie path (CurrentUser yields a UserProfile, not a
    // DeveloperAccount). is_admin lives on UserProfile only.
    let profile = state
        .user_repo
        .get_profile_by_id(found.user.id)
        .await?
        .ok_or(AppError::InvalidCredentials)?;
    if profile.is_frozen {
        return Err(AppError::AccountFrozen);
    }

    // Best-effort touch of last-used. Don't block the request if
    // the update fails.
    if let Err(e) = state
        .developer_auth_repo
        .touch_api_key_last_used(found.api_key.id)
        .await
    {
        tracing::warn!(
            api_key_id = %found.api_key.id,
            error = %e,
            "touch_api_key_last_used failed"
        );
    }

    Ok(Some(profile))
}

/// Extracts the current user and additionally requires `is_admin`.
/// Cookie-only by design — admin operations don't accept API keys.
pub struct AdminUser(pub UserProfile);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let CurrentUser(profile) = CurrentUser::from_request_parts(parts, state).await?;
        if !profile.is_admin {
            return Err(AppError::Forbidden);
        }
        Ok(AdminUser(profile))
    }
}
