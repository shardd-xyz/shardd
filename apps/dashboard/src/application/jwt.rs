use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::app_error::{AppError, AppResult};
use secrecy::ExposeSecret;

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: i64,
    pub iat: i64,
}

pub fn issue(user_id: Uuid, secret: &secrecy::SecretString, ttl: Duration) -> AppResult<String> {
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let exp = now + ttl.whole_seconds();
    let claims = Claims {
        sub: user_id.to_string(),
        iat: now,
        exp,
    };
    let header = Header::new(Algorithm::HS256);
    encode(
        &header,
        &claims,
        &EncodingKey::from_secret(secret.expose_secret().as_bytes()),
    )
    .map_err(|e| AppError::Internal(e.to_string()))
}

pub fn verify(token: &str, secret: &secrecy::SecretString) -> AppResult<Claims> {
    let validation = Validation::new(Algorithm::HS256);
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.expose_secret().as_bytes()),
        &validation,
    )
    .map(|data| data.claims)
    .map_err(|e| AppError::Internal(e.to_string()))
}
