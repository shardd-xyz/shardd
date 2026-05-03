use jsonwebtoken::{
    Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, decode_header, encode,
};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::app_error::{AppError, AppResult};

const SESSION_KID: &str = "dashboard-session";
const SESSION_SCOPE: &str = "me_routes";

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: i64,
    pub iat: i64,
    pub scope: String,
}

pub fn issue(user_id: Uuid, secret: &secrecy::SecretString, ttl: Duration) -> AppResult<String> {
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let claims = Claims {
        sub: user_id.to_string(),
        iat: now,
        exp: now + ttl.whole_seconds(),
        scope: SESSION_SCOPE.to_string(),
    };
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(SESSION_KID.to_string());
    encode(
        &header,
        &claims,
        &EncodingKey::from_secret(secret.expose_secret().as_bytes()),
    )
    .map_err(|e| AppError::Internal(e.to_string()))
}

pub fn verify(token: &str, secret: &secrecy::SecretString) -> AppResult<Uuid> {
    let header = decode_header(token).map_err(|e| AppError::Internal(e.to_string()))?;
    if header.kid.as_deref() != Some(SESSION_KID) {
        return Err(AppError::InvalidCredentials);
    }
    let validation = Validation::new(Algorithm::HS256);
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.expose_secret().as_bytes()),
        &validation,
    )
    .map_err(|_| AppError::InvalidCredentials)?;
    if data.claims.scope != SESSION_SCOPE {
        return Err(AppError::InvalidCredentials);
    }
    Uuid::parse_str(&data.claims.sub).map_err(|_| AppError::InvalidCredentials)
}

pub fn has_dashboard_session_kid(token: &str) -> bool {
    decode_header(token)
        .ok()
        .and_then(|header| header.kid)
        .as_deref()
        == Some(SESSION_KID)
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::SecretString;

    #[test]
    fn session_token_round_trips_user_id() {
        let secret = SecretString::new("test-session-secret".into());
        let user_id = Uuid::new_v4();
        let token = issue(user_id, &secret, Duration::minutes(5)).expect("issue token");

        assert!(has_dashboard_session_kid(&token));
        assert_eq!(verify(&token, &secret).expect("verify token"), user_id);
    }

    #[test]
    fn wrong_secret_rejects_session_token() {
        let user_id = Uuid::new_v4();
        let token = issue(
            user_id,
            &SecretString::new("issuer-secret".into()),
            Duration::minutes(5),
        )
        .expect("issue token");

        assert!(verify(&token, &SecretString::new("other-secret".into())).is_err());
    }
}
