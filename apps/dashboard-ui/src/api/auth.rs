use crate::api::{ApiError, api_delete, api_get, api_post, api_post_no_body, api_post_with_body};
use crate::types::Session;
use serde::{Deserialize, Serialize};

pub async fn verify() -> Result<Session, ApiError> {
    api_get("/api/auth/verify").await
}

#[derive(Serialize)]
struct LoginRequest {
    email: String,
}

pub async fn request_magic_link(email: &str) -> Result<(), ApiError> {
    let body = LoginRequest {
        email: email.to_string(),
    };
    api_post_with_body("/api/auth/request", &body).await
}

#[derive(Serialize)]
struct ConsumeRequest {
    token: String,
}

pub async fn consume_magic_link(token: &str) -> Result<(), ApiError> {
    let body = ConsumeRequest {
        token: token.to_string(),
    };
    api_post_with_body("/api/auth/consume", &body).await
}

pub async fn logout() -> Result<(), ApiError> {
    api_post_no_body("/api/auth/logout").await
}

pub async fn delete_account() -> Result<(), ApiError> {
    api_delete("/api/user/delete").await
}

#[derive(Serialize)]
pub struct ContactRequest {
    pub topic: String,
    pub company: Option<String>,
    pub team_size: Option<String>,
    pub volume: Option<String>,
    pub message: String,
}

pub async fn send_contact(req: &ContactRequest) -> Result<(), ApiError> {
    api_post_with_body("/api/user/contact", req).await
}

#[derive(Serialize)]
struct CliAuthorizeRequest {
    session_id: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct CliAuthorizeResponse {
    pub verification_code: String,
    pub client_name: String,
    pub hostname: String,
}

/// POST /api/auth/cli/authorize — exchanges a CLI device-flow session_id
/// for a one-time verification_code, scoped to the currently-logged-in
/// user. The user pastes the returned code into the waiting CLI; the
/// CLI then exchanges it for a real API key via /api/auth/cli/exchange.
pub async fn cli_authorize(session_id: &str) -> Result<CliAuthorizeResponse, ApiError> {
    let body = CliAuthorizeRequest {
        session_id: session_id.to_string(),
    };
    api_post::<CliAuthorizeResponse>("/api/auth/cli/authorize", &body).await
}
