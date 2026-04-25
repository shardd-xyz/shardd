use crate::api::{
    ApiError, api_delete, api_get, api_get_text, api_patch, api_post, api_post_no_body,
};
use crate::types::*;
use serde::Serialize;

pub async fn me() -> Result<DeveloperProfile, ApiError> {
    api_get("/api/developer/me").await
}

#[derive(Serialize)]
struct UpdateProfileBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<&'a str>,
}

pub async fn update_profile(
    display_name: Option<&str>,
    language: Option<&str>,
) -> Result<serde_json::Value, ApiError> {
    api_patch(
        "/api/user",
        &UpdateProfileBody {
            display_name,
            language,
        },
    )
    .await
}

/// Raw JSON string for the /api/user/export endpoint — fed directly into
/// a Blob download on the Profile page.
pub async fn export_user_data_raw() -> Result<String, ApiError> {
    api_get_text("/api/user/export").await
}

pub async fn list_keys() -> Result<Vec<ApiKey>, ApiError> {
    api_get("/api/developer/keys").await
}

#[derive(Serialize)]
pub struct CreateKeyRequest {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    pub scopes: Vec<CreateScopeRequest>,
}

pub async fn create_key(req: &CreateKeyRequest) -> Result<IssuedKeyResponse, ApiError> {
    api_post("/api/developer/keys", req).await
}

#[allow(dead_code)]
pub async fn rotate_key(key_id: &str) -> Result<IssuedKeyResponse, ApiError> {
    api_post(
        &format!("/api/developer/keys/{key_id}/rotate"),
        &serde_json::json!({}),
    )
    .await
}

pub async fn revoke_key(key_id: &str) -> Result<(), ApiError> {
    api_post_no_body(&format!("/api/developer/keys/{key_id}/revoke")).await
}

pub async fn list_key_scopes(key_id: &str) -> Result<Vec<ApiKeyScope>, ApiError> {
    api_get(&format!("/api/developer/keys/{key_id}/scopes")).await
}

#[derive(Serialize, Clone)]
pub struct CreateScopeRequest {
    /// "bucket" (default) or "control". Backend defaults to "bucket"
    /// when this field is missing, so older payloads still work.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    pub match_type: String,
    #[serde(rename = "bucket")]
    pub resource_value: Option<String>,
    pub can_read: bool,
    pub can_write: bool,
}

pub async fn create_scope(key_id: &str, req: &CreateScopeRequest) -> Result<ApiKeyScope, ApiError> {
    api_post(&format!("/api/developer/keys/{key_id}/scopes"), req).await
}

pub async fn delete_scope(scope_id: &str) -> Result<(), ApiError> {
    api_delete(&format!("/api/developer/scopes/{scope_id}")).await
}
