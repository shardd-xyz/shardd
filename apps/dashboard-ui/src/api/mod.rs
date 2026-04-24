pub mod admin;
pub mod auth;
pub mod billing;
pub mod buckets;
pub mod developer;
pub mod events;

use gloo_net::http::Request;
use serde::de::DeserializeOwned;
use web_sys::RequestCredentials;

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
    /// Server-provided `code` from the JSON error envelope (e.g. "CONFLICT").
    /// Absent on transport errors or non-JSON bodies.
    pub code: Option<String>,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "API error {}: {}", self.status, self.message)
    }
}

impl ApiError {
    /// Translate the server's error envelope into a human-friendly (title,
    /// body) pair for the Notice banner. Generic "Something went wrong" is
    /// reserved for unknown codes; specific codes get specific copy.
    pub fn friendly(&self) -> (&'static str, String) {
        let detail = self.message.clone();
        match self.code.as_deref() {
            Some("CONFLICT") => (
                "Name taken",
                if detail.is_empty() {
                    "Pick another name and try again.".into()
                } else {
                    detail
                },
            ),
            Some("INVALID_INPUT") => (
                "Invalid input",
                if detail.is_empty() {
                    "Double-check your input and try again.".into()
                } else {
                    detail
                },
            ),
            Some("ACCOUNT_FROZEN") => (
                "Account frozen",
                "Contact support to restore access.".into(),
            ),
            Some("RATE_LIMITED") => (
                "Slow down",
                "You're hitting the rate limit \u{2014} try again in a few seconds.".into(),
            ),
            Some("NOT_FOUND") => ("Not found", "It may have been deleted.".into()),
            Some("FORBIDDEN") => (
                "Blocked",
                "Your API key doesn't have permission for this action. Check its scopes.".into(),
            ),
            Some("INVALID_CREDENTIALS") => (
                "Sign in required",
                "Your session has expired \u{2014} sign in again.".into(),
            ),
            Some("INTERNAL_ERROR") | Some("DATABASE_ERROR") => (
                "Something went wrong",
                "Try again in a moment. If it keeps failing, contact support.".into(),
            ),
            _ => (
                "Something went wrong",
                if detail.is_empty() {
                    "An unexpected error occurred. Try again in a moment.".into()
                } else {
                    detail
                },
            ),
        }
    }
}

/// Parse a JSON error envelope (`{"code":"X","message":"Y"}`) out of a text
/// body. Falls back to using the raw text as the message if it isn't JSON.
pub fn parse_api_error(status: u16, body: String) -> ApiError {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
        let code = value
            .get("code")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let message = value
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        return ApiError {
            status,
            message,
            code,
        };
    }
    ApiError {
        status,
        message: body,
        code: None,
    }
}

fn transport_err(e: impl ToString) -> ApiError {
    ApiError {
        status: 0,
        message: e.to_string(),
        code: None,
    }
}

fn decode_err(status: u16, e: impl ToString) -> ApiError {
    ApiError {
        status,
        message: e.to_string(),
        code: None,
    }
}

pub async fn api_get<T: DeserializeOwned>(path: &str) -> Result<T, ApiError> {
    let response = Request::get(path)
        .credentials(RequestCredentials::Include)
        .send()
        .await
        .map_err(transport_err)?;

    if !response.ok() {
        let status = response.status();
        return Err(parse_api_error(
            status,
            response.text().await.unwrap_or_default(),
        ));
    }

    let status = response.status();
    response
        .json::<T>()
        .await
        .map_err(|e| decode_err(status, e))
}

pub async fn api_post<T: DeserializeOwned>(
    path: &str,
    body: &impl serde::Serialize,
) -> Result<T, ApiError> {
    let response = Request::post(path)
        .credentials(RequestCredentials::Include)
        .json(body)
        .map_err(transport_err)?
        .send()
        .await
        .map_err(transport_err)?;

    if !response.ok() {
        let status = response.status();
        return Err(parse_api_error(
            status,
            response.text().await.unwrap_or_default(),
        ));
    }

    let status = response.status();
    response
        .json::<T>()
        .await
        .map_err(|e| decode_err(status, e))
}

pub async fn api_post_no_body(path: &str) -> Result<(), ApiError> {
    let response = Request::post(path)
        .credentials(RequestCredentials::Include)
        .send()
        .await
        .map_err(transport_err)?;

    if !response.ok() {
        let status = response.status();
        return Err(parse_api_error(
            status,
            response.text().await.unwrap_or_default(),
        ));
    }

    Ok(())
}

pub async fn api_post_with_body<B: serde::Serialize>(path: &str, body: &B) -> Result<(), ApiError> {
    let response = Request::post(path)
        .credentials(RequestCredentials::Include)
        .json(body)
        .map_err(transport_err)?
        .send()
        .await
        .map_err(transport_err)?;

    if !response.ok() {
        let status = response.status();
        return Err(parse_api_error(
            status,
            response.text().await.unwrap_or_default(),
        ));
    }

    Ok(())
}

pub async fn api_patch<B: serde::Serialize, T: DeserializeOwned>(
    path: &str,
    body: &B,
) -> Result<T, ApiError> {
    let response = Request::patch(path)
        .credentials(RequestCredentials::Include)
        .json(body)
        .map_err(transport_err)?
        .send()
        .await
        .map_err(transport_err)?;

    if !response.ok() {
        let status = response.status();
        return Err(parse_api_error(
            status,
            response.text().await.unwrap_or_default(),
        ));
    }

    let status = response.status();
    response
        .json::<T>()
        .await
        .map_err(|e| decode_err(status, e))
}

pub async fn api_get_text(path: &str) -> Result<String, ApiError> {
    let response = Request::get(path)
        .credentials(RequestCredentials::Include)
        .send()
        .await
        .map_err(transport_err)?;
    if !response.ok() {
        let status = response.status();
        return Err(parse_api_error(
            status,
            response.text().await.unwrap_or_default(),
        ));
    }
    response.text().await.map_err(|e| decode_err(0, e))
}

pub async fn api_delete(path: &str) -> Result<(), ApiError> {
    let response = Request::delete(path)
        .credentials(RequestCredentials::Include)
        .send()
        .await
        .map_err(transport_err)?;

    if !response.ok() {
        let status = response.status();
        return Err(parse_api_error(
            status,
            response.text().await.unwrap_or_default(),
        ));
    }

    Ok(())
}
