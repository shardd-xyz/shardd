use axum::{extract::FromRequestParts, http::request::Parts};
use axum_extra::extract::cookie::CookieJar;
use uuid::Uuid;

use crate::{
    adapters::http::app_state::AppState, app_error::AppError, application::jwt,
    use_cases::user::UserProfile,
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

/// Extracts the current user and additionally requires `is_admin`.
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
