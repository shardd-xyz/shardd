use axum::{extract::FromRequestParts, http::request::Parts};
use jsonwebtoken::{DecodingKey, Validation, decode};
use secrecy::ExposeSecret;
use serde::Deserialize;
use uuid::Uuid;

use crate::adapters::http::app_state::AppState;
use crate::application::app_error::AppError;

#[derive(Debug, Deserialize)]
struct Claims {
    sub: Uuid,
}

/// Extracts an authenticated user_id from the access_token cookie (same JWT secret as dashboard).
pub struct AuthUser(pub Uuid);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let cookie_header = parts
            .headers
            .get(axum::http::header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        let token = cookie_header
            .split(';')
            .filter_map(|c| {
                let c = c.trim();
                c.strip_prefix("access_token=")
            })
            .next()
            .ok_or(AppError::Unauthorized)?;

        let secret = state.config.jwt_secret.expose_secret();
        let key = DecodingKey::from_secret(secret.as_bytes());
        let mut validation = Validation::default();
        validation.validate_exp = true;

        let data =
            decode::<Claims>(token, &key, &validation).map_err(|_| AppError::Unauthorized)?;

        Ok(AuthUser(data.claims.sub))
    }
}

/// Authenticates calls from internal services (e.g. the dashboard admin proxy)
/// by comparing the `Authorization: Bearer <secret>` header against
/// `billing_internal_secret`.
pub struct MachineAuth;

impl FromRequestParts<AppState> for MachineAuth {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(AppError::Unauthorized)?;
        let provided = header
            .strip_prefix("Bearer ")
            .ok_or(AppError::Unauthorized)?
            .as_bytes();
        let expected = state
            .config
            .billing_internal_secret
            .expose_secret()
            .as_bytes();
        if expected.len() != provided.len() {
            return Err(AppError::Unauthorized);
        }
        let mut diff: u8 = 0;
        for (a, b) in expected.iter().zip(provided.iter()) {
            diff |= a ^ b;
        }
        if diff != 0 {
            return Err(AppError::Unauthorized);
        }
        Ok(MachineAuth)
    }
}
