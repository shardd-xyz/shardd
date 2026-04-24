use crate::api::{ApiError, api_delete, api_get, api_post_no_body, api_post_with_body};
use crate::types::Session;
use serde::Serialize;

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
