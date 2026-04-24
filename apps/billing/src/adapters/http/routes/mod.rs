use axum::Router;

use crate::adapters::http::app_state::AppState;

pub mod admin;
pub mod billing;
pub mod webhooks;

pub fn router() -> Router<AppState> {
    Router::new()
        .nest("/billing", billing::router())
        .nest("/webhooks", webhooks::router())
}

/// Router for internal cross-service calls (e.g. from the dashboard admin
/// proxy). Mounted at `/internal` outside the public `/api` tree so Caddy's
/// `/api/*` public reverse-proxy never reaches these routes.
pub fn internal_router() -> Router<AppState> {
    Router::new().nest("/admin", admin::router())
}
