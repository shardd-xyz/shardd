use axum::{Router, http, middleware};
use http::header::{ACCEPT_LANGUAGE, AUTHORIZATION, CONTENT_TYPE};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use uuid::Uuid;

use crate::{
    adapters::{
        self,
        http::{app_state::AppState, middleware::rate_limit_middleware},
    },
    infra::setup::init_tracing,
};

pub fn create_app(app_state: AppState) -> Router {
    init_tracing();

    let cors = CorsLayer::new()
        .allow_origin(app_state.config.cors_origin.clone())
        .allow_methods([http::Method::GET, http::Method::POST, http::Method::DELETE])
        .allow_headers([CONTENT_TYPE, AUTHORIZATION, ACCEPT_LANGUAGE])
        .allow_credentials(true);

    Router::new()
        .nest("/api", adapters::http::routes::router())
        .with_state(app_state.clone())
        .layer(middleware::from_fn_with_state(
            app_state,
            rate_limit_middleware,
        ))
        .layer(cors)
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &http::Request<_>| {
                let request_id = Uuid::new_v4();
                tracing::info_span!(
                    "http-request",
                    method = %request.method(),
                    uri = %request.uri(),
                    version = ?request.version(),
                    request_id = %request_id
                )
            }),
        )
}
