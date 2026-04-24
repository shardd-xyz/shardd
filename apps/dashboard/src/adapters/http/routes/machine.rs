use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};

use crate::{
    adapters::http::app_state::AppState,
    app_error::{AppError, AppResult},
    use_cases::developer_auth::{MachineAction, MachineAuthDecision},
};

const MACHINE_SECRET_HEADER: &str = "x-machine-auth-secret";

pub fn router() -> Router<AppState> {
    Router::new().route("/introspect", post(introspect))
}

#[derive(Deserialize)]
struct MachineIntrospectRequest {
    api_key: String,
    action: MachineAction,
    bucket: String,
}

#[derive(Serialize)]
struct MachineIntrospectResponse {
    decision: MachineAuthDecision,
}

async fn introspect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<MachineIntrospectRequest>,
) -> AppResult<Json<MachineIntrospectResponse>> {
    authorize_machine_caller(&state, &headers)?;
    let decision = state
        .developer_auth_use_cases
        .introspect(&request.api_key, request.action, &request.bucket)
        .await?;
    Ok(Json(MachineIntrospectResponse { decision }))
}

fn authorize_machine_caller(state: &AppState, headers: &HeaderMap) -> AppResult<()> {
    let configured = state
        .config
        .machine_auth_shared_secret
        .as_ref()
        .ok_or_else(|| AppError::Internal("machine auth shared secret is not configured".into()))?;
    let provided = headers
        .get(MACHINE_SECRET_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or(AppError::Forbidden)?;
    if provided != configured.expose_secret() {
        return Err(AppError::Forbidden);
    }
    Ok(())
}
