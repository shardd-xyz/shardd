use dotenvy::dotenv;
use tracing::info;

use shardd_billing::infra::{app::create_app, notifications, setup::init_app_state};
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();

    let app_state = init_app_state().await?;
    let bind_addr = app_state.config.bind_addr;

    // Spawn background task for low-balance email notifications
    notifications::spawn_notification_loop(app_state.clone());

    let app = create_app(app_state);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;

    info!(listen = %listener.local_addr()?, "shardd billing listening");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
