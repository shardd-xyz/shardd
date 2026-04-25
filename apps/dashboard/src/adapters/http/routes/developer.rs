use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    adapters::http::{app_state::AppState, extractors::Authenticated},
    app_error::{AppError, AppResult},
    use_cases::{
        developer_auth::{
            DeveloperAccount, DeveloperApiKey, DeveloperApiKeyScope, IssuedApiKey, NewScope,
            ScopeMatchType, ScopeResourceType,
        },
        user::UserProfile,
    },
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me", get(me))
        .route("/keys", get(list_keys).post(create_key))
        .route("/keys/{id}/revoke", post(revoke_key))
        .route("/keys/{id}/rotate", post(rotate_key))
        .route("/keys/{id}/scopes", get(list_scopes).post(create_scope))
        .route("/scopes/{id}", delete(delete_scope))
}

#[derive(Deserialize)]
struct CreateKeyRequest {
    name: String,
    expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    scopes: Vec<CreateScopeRequest>,
}

#[derive(Serialize)]
struct IssuedKeyResponse {
    api_key: DeveloperApiKey,
    raw_key: String,
}

#[derive(Deserialize, Clone)]
struct CreateScopeRequest {
    match_type: ScopeMatchType,
    bucket: Option<String>,
    #[serde(default)]
    can_read: bool,
    #[serde(default)]
    can_write: bool,
    /// Optional, defaults to `bucket` for backward compatibility with
    /// the dashboard's existing scope-create payload.
    #[serde(default = "default_scope_resource_type")]
    resource_type: ScopeResourceType,
}

fn default_scope_resource_type() -> ScopeResourceType {
    ScopeResourceType::Bucket
}

async fn me(
    Authenticated(user): Authenticated,
    State(state): State<AppState>,
) -> AppResult<Json<DeveloperAccount>> {
    Ok(Json(owned_account(&state, &user).await?))
}

async fn list_keys(
    Authenticated(user): Authenticated,
    State(state): State<AppState>,
) -> AppResult<Json<Vec<DeveloperApiKey>>> {
    let account = owned_account(&state, &user).await?;
    Ok(Json(
        state.developer_auth_repo.list_api_keys(account.id).await?,
    ))
}

async fn create_key(
    Authenticated(user): Authenticated,
    State(state): State<AppState>,
    Json(request): Json<CreateKeyRequest>,
) -> AppResult<(StatusCode, Json<IssuedKeyResponse>)> {
    let account = mutable_owned_account(&state, &user).await?;
    for scope in &request.scopes {
        validate_scope_request(scope)?;
    }
    let scopes: Vec<NewScope> = request
        .scopes
        .iter()
        .map(|s| NewScope {
            resource_type: s.resource_type.clone(),
            match_type: s.match_type.clone(),
            resource_value: s.bucket.clone(),
            can_read: s.can_read,
            can_write: s.can_write,
        })
        .collect();
    let issued = state
        .developer_auth_use_cases
        .issue_api_key_with_scopes(account.id, &request.name, request.expires_at, &scopes)
        .await?;
    Ok((StatusCode::CREATED, Json(issued.into())))
}

async fn revoke_key(
    Authenticated(user): Authenticated,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let account = mutable_owned_account(&state, &user).await?;
    let _ = owned_api_key(&state, &account, id).await?;
    state.developer_auth_repo.revoke_api_key(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn rotate_key(
    Authenticated(user): Authenticated,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<(StatusCode, Json<IssuedKeyResponse>)> {
    let account = mutable_owned_account(&state, &user).await?;
    let _ = owned_api_key(&state, &account, id).await?;
    let issued = state.developer_auth_use_cases.rotate_api_key(id).await?;
    Ok((StatusCode::CREATED, Json(issued.into())))
}

async fn list_scopes(
    Authenticated(user): Authenticated,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Vec<DeveloperApiKeyScope>>> {
    let account = owned_account(&state, &user).await?;
    let _ = owned_api_key(&state, &account, id).await?;
    Ok(Json(state.developer_auth_repo.list_scopes(id).await?))
}

async fn create_scope(
    Authenticated(user): Authenticated,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(request): Json<CreateScopeRequest>,
) -> AppResult<(StatusCode, Json<DeveloperApiKeyScope>)> {
    let account = mutable_owned_account(&state, &user).await?;
    let _ = owned_api_key(&state, &account, id).await?;
    validate_scope_request(&request)?;
    let scope = state
        .developer_auth_repo
        .create_scope(
            id,
            request.resource_type.clone(),
            request.match_type,
            request.bucket.as_deref(),
            request.can_read,
            request.can_write,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(scope)))
}

async fn delete_scope(
    Authenticated(user): Authenticated,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let account = mutable_owned_account(&state, &user).await?;
    let scope = state
        .developer_auth_repo
        .get_scope(id)
        .await?
        .ok_or(AppError::NotFound)?;
    let _ = owned_api_key(&state, &account, scope.api_key_id).await?;
    state.developer_auth_repo.delete_scope(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

impl From<IssuedApiKey> for IssuedKeyResponse {
    fn from(value: IssuedApiKey) -> Self {
        Self {
            api_key: value.api_key,
            raw_key: value.raw_key,
        }
    }
}

async fn owned_account(state: &AppState, user: &UserProfile) -> AppResult<DeveloperAccount> {
    state.developer_auth_use_cases.get_account(user.id).await
}

async fn mutable_owned_account(
    state: &AppState,
    user: &UserProfile,
) -> AppResult<DeveloperAccount> {
    let account = owned_account(state, user).await?;
    if account.is_frozen {
        return Err(AppError::Conflict("user account is frozen".into()));
    }
    Ok(account)
}

async fn owned_api_key(
    state: &AppState,
    account: &DeveloperAccount,
    api_key_id: Uuid,
) -> AppResult<DeveloperApiKey> {
    let key = state
        .developer_auth_repo
        .get_api_key(api_key_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if key.user_id != account.id {
        return Err(AppError::NotFound);
    }
    Ok(key)
}

fn validate_scope_request(request: &CreateScopeRequest) -> AppResult<()> {
    if !request.can_read && !request.can_write {
        return Err(AppError::InvalidInput(
            "scope must grant read or write".into(),
        ));
    }
    match request.resource_type {
        ScopeResourceType::Bucket => {
            if request.match_type != ScopeMatchType::All
                && request
                    .bucket
                    .as_deref()
                    .is_none_or(|bucket| bucket.trim().is_empty())
            {
                return Err(AppError::InvalidInput(
                    "bucket is required for exact or prefix scopes".into(),
                ));
            }
        }
        ScopeResourceType::Control => {
            if request.match_type != ScopeMatchType::All
                || request
                    .bucket
                    .as_deref()
                    .is_some_and(|b| !b.trim().is_empty())
            {
                return Err(AppError::InvalidInput(
                    "control scopes must use match_type=all with no bucket".into(),
                ));
            }
        }
    }
    Ok(())
}
