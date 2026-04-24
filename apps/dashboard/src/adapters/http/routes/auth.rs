use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use time;
use uuid::Uuid;

use crate::{
    adapters::http::app_state::AppState,
    app_error::{AppError, AppResult},
    application::jwt,
    use_cases::user::AuthUseCases,
};

#[derive(Deserialize)]
struct RequestPayload {
    email: String,
}

#[derive(Deserialize)]
struct ConsumePayload {
    token: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/request", post(request))
        .route("/consume", post(consume))
        .route("/verify", get(verify))
        .route("/logout", post(logout))
        .route("/google", get(google_redirect))
        .route("/google/callback", get(google_callback))
}

async fn request(
    State(app_state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(payload): Json<RequestPayload>,
) -> AppResult<impl IntoResponse> {
    let (jar, session_id) = ensure_login_session(jar, app_state.config.magic_link_ttl_minutes);
    let auth: Arc<AuthUseCases> = app_state.auth_use_cases.clone();
    let language = headers
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok());
    auth.request_magic_link(
        &payload.email,
        &session_id,
        app_state.config.magic_link_ttl_minutes,
        language,
    )
    .await?;
    Ok((StatusCode::ACCEPTED, jar))
}

async fn consume(
    State(app_state): State<AppState>,
    jar: CookieJar,
    Json(payload): Json<ConsumePayload>,
) -> AppResult<impl IntoResponse> {
    let Some(session_cookie) = jar.get("login_session") else {
        return Ok((StatusCode::UNAUTHORIZED, HeaderMap::new()));
    };
    let session_id = session_cookie.value().to_owned();

    let auth = app_state.auth_use_cases.clone();
    if let Some(user_id) = auth.consume_magic_link(&payload.token, &session_id).await? {
        // Get user email
        let Some(profile) = app_state.user_repo.get_profile_by_id(user_id).await? else {
            return Ok((StatusCode::UNAUTHORIZED, HeaderMap::new()));
        };

        if profile.is_frozen {
            return Err(AppError::AccountFrozen);
        }

        // Auto-grant admin if email is in ADMIN_EMAILS list.
        let email_lc = profile.email.to_lowercase();
        if !profile.is_admin && app_state.config.admin_emails.contains(&email_lc) {
            app_state.user_repo.set_admin(user_id, true).await?;
        }
        app_state.user_repo.touch_last_login(user_id).await?;

        let email = profile.email;

        let access = jwt::issue(
            user_id,
            &app_state.config.jwt_secret,
            app_state.config.access_token_ttl,
        )?;
        let refresh = jwt::issue(
            user_id,
            &app_state.config.jwt_secret,
            app_state.config.refresh_token_ttl,
        )?;

        let mut headers = HeaderMap::new();
        let access_cookie = Cookie::build(("access_token", access))
            .http_only(true)
            .same_site(SameSite::Lax)
            .path("/")
            .max_age(app_state.config.access_token_ttl)
            .build();
        let refresh_cookie = Cookie::build(("refresh_token", refresh))
            .http_only(true)
            .same_site(SameSite::Lax)
            .path("/")
            .max_age(app_state.config.refresh_token_ttl)
            .build();
        let email_cookie = Cookie::build(("user_email", email))
            .http_only(false)
            .same_site(SameSite::Lax)
            .path("/")
            .build();
        headers.append("set-cookie", access_cookie.to_string().parse().unwrap());
        headers.append("set-cookie", refresh_cookie.to_string().parse().unwrap());
        headers.append("set-cookie", email_cookie.to_string().parse().unwrap());
        return Ok((StatusCode::OK, headers));
    }
    let headers = HeaderMap::new();
    Ok((StatusCode::UNAUTHORIZED, headers))
}

#[derive(Serialize)]
struct VerifyResponse {
    id: uuid::Uuid,
    email: String,
    is_admin: bool,
}

async fn verify(
    cookies: CookieJar,
    State(app_state): State<AppState>,
) -> AppResult<Json<VerifyResponse>> {
    let access = cookies
        .get("access_token")
        .ok_or(AppError::InvalidCredentials)?;
    let claims = jwt::verify(access.value(), &app_state.config.jwt_secret)?;
    let user_id = uuid::Uuid::parse_str(&claims.sub).map_err(|_| AppError::InvalidCredentials)?;
    let profile = app_state
        .user_repo
        .get_profile_by_id(user_id)
        .await?
        .ok_or(AppError::InvalidCredentials)?;
    if profile.is_frozen {
        return Err(AppError::AccountFrozen);
    }
    Ok(Json(VerifyResponse {
        id: profile.id,
        email: profile.email,
        is_admin: profile.is_admin,
    }))
}

async fn logout() -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    for (name, http_only) in [
        ("access_token", true),
        ("refresh_token", true),
        ("user_email", false),
        ("impersonating", false),
        ("login_session", true),
    ] {
        let c = Cookie::build((name, ""))
            .http_only(http_only)
            .same_site(SameSite::Lax)
            .path("/")
            .max_age(time::Duration::seconds(0))
            .build();
        headers.append("set-cookie", c.to_string().parse().unwrap());
    }
    (StatusCode::OK, headers)
}

async fn google_redirect(State(app_state): State<AppState>) -> Response {
    let Some(client_id) = &app_state.config.google_client_id else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Google login not configured",
        )
            .into_response();
    };
    let redirect_uri = format!(
        "{}/api/auth/google/callback",
        app_state.config.app_origin.as_str().trim_end_matches('/')
    );
    let state = Uuid::new_v4().to_string();
    let url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope=email+profile&state={}&access_type=online&prompt=select_account",
        urlencoding::encode(client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(&state),
    );
    Redirect::temporary(&url).into_response()
}

#[derive(Deserialize)]
struct GoogleCallbackQuery {
    code: String,
    #[allow(dead_code)]
    state: Option<String>,
}

#[derive(Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
struct GoogleUserInfo {
    email: String,
}

async fn google_callback(
    State(app_state): State<AppState>,
    Query(query): Query<GoogleCallbackQuery>,
) -> Response {
    let (Some(client_id), Some(client_secret)) = (
        &app_state.config.google_client_id,
        &app_state.config.google_client_secret,
    ) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Google login not configured",
        )
            .into_response();
    };
    let redirect_uri = format!(
        "{}/api/auth/google/callback",
        app_state.config.app_origin.as_str().trim_end_matches('/')
    );

    let token_res = reqwest::Client::new()
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("code", query.code.as_str()),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.expose_secret()),
            ("redirect_uri", redirect_uri.as_str()),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await;

    let token_res = match token_res {
        Ok(res) => res,
        Err(e) => {
            tracing::error!("Google token exchange failed: {e}");
            return Redirect::temporary("/login").into_response();
        }
    };

    let token_data: GoogleTokenResponse = match token_res.json().await {
        Ok(data) => data,
        Err(e) => {
            tracing::error!("Google token parse failed: {e}");
            return Redirect::temporary("/login").into_response();
        }
    };

    let user_res = reqwest::Client::new()
        .get("https://www.googleapis.com/oauth2/v2/userinfo")
        .bearer_auth(&token_data.access_token)
        .send()
        .await;

    let user_info: GoogleUserInfo = match user_res {
        Ok(res) => match res.json().await {
            Ok(info) => info,
            Err(e) => {
                tracing::error!("Google userinfo parse failed: {e}");
                return Redirect::temporary("/login").into_response();
            }
        },
        Err(e) => {
            tracing::error!("Google userinfo request failed: {e}");
            return Redirect::temporary("/login").into_response();
        }
    };

    let email = user_info.email.to_lowercase();
    let profile = match app_state.user_repo.upsert_by_email(&email, None).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("User upsert failed for Google login: {e}");
            return Redirect::temporary("/login").into_response();
        }
    };

    if profile.is_frozen {
        return Redirect::temporary("/login").into_response();
    }

    let email_lc = profile.email.to_lowercase();
    if !profile.is_admin && app_state.config.admin_emails.contains(&email_lc) {
        let _ = app_state.user_repo.set_admin(profile.id, true).await;
    }
    let _ = app_state.user_repo.touch_last_login(profile.id).await;

    let access = match jwt::issue(
        profile.id,
        &app_state.config.jwt_secret,
        app_state.config.access_token_ttl,
    ) {
        Ok(t) => t,
        Err(_) => return Redirect::temporary("/login").into_response(),
    };
    let refresh = match jwt::issue(
        profile.id,
        &app_state.config.jwt_secret,
        app_state.config.refresh_token_ttl,
    ) {
        Ok(t) => t,
        Err(_) => return Redirect::temporary("/login").into_response(),
    };

    let mut headers = HeaderMap::new();
    for (name, value, http_only, max_age) in [
        (
            "access_token",
            access.as_str(),
            true,
            Some(app_state.config.access_token_ttl),
        ),
        (
            "refresh_token",
            refresh.as_str(),
            true,
            Some(app_state.config.refresh_token_ttl),
        ),
        ("user_email", &profile.email, false, None),
    ] {
        let mut builder = Cookie::build((name, value.to_owned()))
            .http_only(http_only)
            .same_site(SameSite::Lax)
            .path("/");
        if let Some(ttl) = max_age {
            builder = builder.max_age(ttl);
        }
        headers.append("set-cookie", builder.build().to_string().parse().unwrap());
    }
    headers.insert(header::LOCATION, "/dashboard".parse().unwrap());
    (StatusCode::FOUND, headers).into_response()
}

fn ensure_login_session(jar: CookieJar, ttl_minutes: i64) -> (CookieJar, String) {
    let session_id = jar
        .get("login_session")
        .map(|c| c.value().to_owned())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let cookie = Cookie::build(("login_session", session_id.clone()))
        .http_only(true)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(time::Duration::minutes(ttl_minutes))
        .build();
    (jar.add(cookie), session_id)
}
