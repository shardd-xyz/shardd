use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Request, State},
    middleware::Next,
    response::Response,
};
use axum_extra::extract::CookieJar;

use crate::{adapters::http::app_state::AppState, app_error::AppError};

pub async fn rate_limit_middleware(
    State(app_state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    cookies: CookieJar,
    mut request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let ip = forwarded_ip(&request).unwrap_or_else(|| addr.ip().to_string());
    let email = cookies.get("user_email").map(|c| c.value().to_owned());

    // Log the IP being used for rate limiting to help debug
    tracing::debug!(
        "Rate limiting - ConnectInfo IP: {}, Forwarded IP: {:?}, Using: {}, Email: {:?}",
        addr.ip(),
        forwarded_ip(&request),
        ip,
        email
    );

    app_state.rate_limiter.check(&ip, email.as_deref()).await?;

    // Preserve cookie jar for downstream extractors.
    request.extensions_mut().insert(cookies);

    Ok(next.run(request).await)
}

fn forwarded_ip(req: &Request) -> Option<String> {
    // Trust X-Forwarded-For / X-Real-IP set by the reverse proxy (nginx).
    if let Some(forwarded) = req.headers().get("x-forwarded-for")
        && let Ok(val) = forwarded.to_str()
        && let Some(first) = val.split(',').next()
    {
        let trimmed = first.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    if let Some(real) = req.headers().get("x-real-ip")
        && let Ok(val) = real.to_str()
        && !val.trim().is_empty()
    {
        return Some(val.trim().to_string());
    }
    None
}
