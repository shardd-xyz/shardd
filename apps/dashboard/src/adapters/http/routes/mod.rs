pub mod admin;
pub mod auth;
pub mod developer;
pub mod developer_auth;
pub mod developer_buckets;
pub mod machine;
pub mod user;

use axum::Router;

use crate::adapters::http::app_state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .nest("/auth", auth::router())
        .nest(
            "/developer",
            developer::router().merge(developer_buckets::router()),
        )
        .nest("/user", user::router())
        .nest("/admin", admin::router().merge(developer_auth::router()))
        .nest("/machine", machine::router())
}
