use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    routing::{delete, patch, post},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use secrecy::ExposeSecret;
use serde::Deserialize;
use time;

use crate::{
    adapters::http::app_state::AppState,
    app_error::{AppError, AppResult},
    application::jwt,
    use_cases::buckets_registry::BucketStatusFilter,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/delete", delete(delete_account))
        .route("/contact", post(send_contact))
        .route("/", patch(update_profile))
        .route("/export", axum::routing::get(export_user_data))
}

async fn delete_account(
    State(app_state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
) -> AppResult<(StatusCode, HeaderMap)> {
    let (_, user_id) = current_user(&jar, &app_state)?;
    let lang = headers
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok());

    app_state
        .auth_use_cases
        .delete_account(user_id, lang)
        .await?;

    let mut headers = HeaderMap::new();
    for (name, value, http_only) in [
        ("access_token", "", true),
        ("refresh_token", "", true),
        ("user_email", "", false),
        ("login_session", "", true),
    ] {
        let cookie = Cookie::build((name, value))
            .http_only(http_only)
            .same_site(SameSite::Lax)
            .path("/")
            .max_age(time::Duration::seconds(0))
            .build();
        headers.append("set-cookie", cookie.to_string().parse().unwrap());
    }

    Ok((StatusCode::NO_CONTENT, headers))
}

#[derive(Deserialize)]
struct UpdateProfileRequest {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    language: Option<String>,
}

#[derive(serde::Serialize)]
struct ProfileDto {
    id: uuid::Uuid,
    email: String,
    display_name: Option<String>,
    language: String,
}

impl From<crate::use_cases::user::UserProfile> for ProfileDto {
    fn from(p: crate::use_cases::user::UserProfile) -> Self {
        Self {
            id: p.id,
            email: p.email,
            display_name: p.display_name,
            language: p.language,
        }
    }
}

async fn update_profile(
    State(app_state): State<AppState>,
    jar: CookieJar,
    Json(body): Json<UpdateProfileRequest>,
) -> AppResult<Json<ProfileDto>> {
    let (_, user_id) = current_user(&jar, &app_state)?;
    if let Some(name) = body.display_name.as_deref() {
        app_state
            .user_repo
            .update_display_name(user_id, Some(name))
            .await?;
    }
    if let Some(lang) = body.language.as_deref() {
        app_state.user_repo.update_language(user_id, lang).await?;
    }
    let user = app_state
        .user_repo
        .get_profile_by_id(user_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(user.into()))
}

/// GET /api/user/export — stream the user's account data (profile, API keys,
/// scopes, bucket registry). Used by the Profile page's "Download data"
/// button. Bucket events intentionally excluded — the mesh is append-only
/// and that's where the auditable record lives.
async fn export_user_data(
    State(app_state): State<AppState>,
    jar: CookieJar,
) -> AppResult<Json<serde_json::Value>> {
    let (_, user_id) = current_user(&jar, &app_state)?;
    let user = app_state
        .user_repo
        .get_profile_by_id(user_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let keys = app_state
        .developer_auth_repo
        .list_api_keys(user_id)
        .await
        .unwrap_or_default();
    let mut keys_out = Vec::with_capacity(keys.len());
    for k in keys.iter() {
        let scopes = app_state
            .developer_auth_repo
            .list_scopes(k.id)
            .await
            .unwrap_or_default();
        keys_out.push(serde_json::json!({
            "id": k.id,
            "name": k.name,
            "prefix": k.key_prefix,
            "created_at": k.created_at.to_rfc3339(),
            "expires_at": k.expires_at.map(|t| t.to_rfc3339()),
            "revoked_at": k.revoked_at.map(|t| t.to_rfc3339()),
            "scopes": scopes,
        }));
    }
    let buckets = app_state
        .bucket_registry
        .list(user_id, BucketStatusFilter::All)
        .await
        .unwrap_or_default();

    Ok(Json(serde_json::json!({
        "exported_at": chrono::Utc::now().to_rfc3339(),
        "user": {
            "id": user.id,
            "email": user.email,
            "display_name": user.display_name,
            "language": user.language,
            "is_admin": user.is_admin,
            "created_at": user.created_at.to_rfc3339(),
            "last_login_at": user.last_login_at.map(|t| t.to_rfc3339()),
        },
        "api_keys": keys_out,
        "buckets": buckets,
    })))
}

#[derive(Deserialize)]
struct ContactRequest {
    topic: String,
    company: Option<String>,
    team_size: Option<String>,
    volume: Option<String>,
    message: String,
}

/// POST /api/user/contact — authenticated user sends a message to the ops
/// inbox. Replaces the old mailto:emil@tqdm.org Enterprise link so people
/// don't have to leave the app (or reveal the personal address).
async fn send_contact(
    State(app_state): State<AppState>,
    jar: CookieJar,
    Json(body): Json<ContactRequest>,
) -> AppResult<StatusCode> {
    let (_, user_id) = current_user(&jar, &app_state)?;
    if body.message.trim().is_empty() {
        return Err(AppError::InvalidInput("message is required".into()));
    }
    let user = app_state
        .user_repo
        .get_profile_by_id(user_id)
        .await?
        .ok_or(AppError::NotFound)?;

    let to = app_state
        .config
        .admin_emails
        .first()
        .cloned()
        .unwrap_or_else(|| app_state.config.email_from.clone());
    let subject = format!("[shardd contact] {}", body.topic);
    let mut html = format!(
        "<p><strong>From:</strong> {email} (user {uid})</p>\
         <p><strong>Topic:</strong> {topic}</p>",
        email = html_escape(&user.email),
        uid = user_id,
        topic = html_escape(&body.topic),
    );
    if let Some(c) = body.company.as_deref().filter(|s| !s.is_empty()) {
        html.push_str(&format!(
            "<p><strong>Company:</strong> {}</p>",
            html_escape(c)
        ));
    }
    if let Some(s) = body.team_size.as_deref().filter(|s| !s.is_empty()) {
        html.push_str(&format!(
            "<p><strong>Team size:</strong> {}</p>",
            html_escape(s)
        ));
    }
    if let Some(v) = body.volume.as_deref().filter(|s| !s.is_empty()) {
        html.push_str(&format!(
            "<p><strong>Volume:</strong> {}</p>",
            html_escape(v)
        ));
    }
    html.push_str(&format!(
        "<p><strong>Message:</strong></p><p>{}</p>",
        html_escape(&body.message).replace('\n', "<br/>")
    ));

    // Spin up a one-shot ResendEmailSender inline rather than adding another
    // field to AppState — the config already carries the API key.
    let sender = crate::adapters::email::resend::ResendEmailSender::new(
        app_state.config.resend_api_key.clone(),
        app_state.config.email_from.clone(),
    );
    <crate::adapters::email::resend::ResendEmailSender as crate::use_cases::user::EmailSender>::send(
        &sender, &to, &subject, &html,
    )
    .await?;
    let _ = app_state.config.resend_api_key.expose_secret(); // silence unused warning pre-compile
    Ok(StatusCode::NO_CONTENT)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn current_user(jar: &CookieJar, app_state: &AppState) -> AppResult<(CookieJar, uuid::Uuid)> {
    let Some(access_cookie) = jar.get("access_token") else {
        return Err(crate::app_error::AppError::InvalidCredentials);
    };
    let claims = jwt::verify(access_cookie.value(), &app_state.config.jwt_secret)?;
    let user_id = uuid::Uuid::parse_str(&claims.sub)
        .map_err(|_| crate::app_error::AppError::InvalidCredentials)?;
    Ok((jar.clone(), user_id))
}
