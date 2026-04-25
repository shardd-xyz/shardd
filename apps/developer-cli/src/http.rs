//! Thin reqwest wrapper for the dashboard's control-plane API. The
//! data-plane goes through `shardd::Client` (the published SDK) — this
//! file only handles `/api/auth/cli/*`, `/api/developer/*`, and
//! `/api/user/*`.

use anyhow::{Context, Result, anyhow};
use reqwest::{Method, StatusCode, header};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone)]
pub struct DashboardClient {
    inner: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
}

impl DashboardClient {
    pub fn new(base_url: String, api_key: Option<String>) -> Result<Self> {
        let inner = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("build reqwest client")?;
        Ok(Self {
            inner,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn authed(&self, mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        req
    }

    /// Fire a request and return either the deserialised body or a
    /// helpful error. 4xx/5xx bodies are surfaced verbatim where
    /// possible.
    pub async fn request_json<R, B>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<R>
    where
        R: for<'de> Deserialize<'de>,
        B: Serialize + ?Sized,
    {
        let raw = self
            .request_raw(method, path, body.map(serialize_body).transpose()?)
            .await?;
        if raw.is_empty() || raw == "null" {
            // Some endpoints 204 on success; only callers that asked
            // for `()` should land here — rely on the deserialiser.
        }
        serde_json::from_str(&raw).with_context(|| format!("decode response body: {raw}"))
    }

    pub async fn request_value(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value> {
        let raw = self
            .request_raw(method, path, body.map(serialize_body).transpose()?)
            .await?;
        if raw.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&raw).with_context(|| format!("decode response body: {raw}"))
    }

    pub async fn request_no_content(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<()> {
        let _ = self
            .request_raw(method, path, body.map(serialize_body).transpose()?)
            .await?;
        Ok(())
    }

    async fn request_raw(
        &self,
        method: Method,
        path: &str,
        body: Option<String>,
    ) -> Result<String> {
        let url = self.url(path);
        let mut req = self.authed(self.inner.request(method.clone(), &url));
        if let Some(b) = body.as_ref() {
            req = req
                .header(header::CONTENT_TYPE, "application/json")
                .body(b.clone());
        }
        let response = req
            .send()
            .await
            .with_context(|| format!("{method} {url}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(map_error(status, &text));
        }
        Ok(text)
    }
}

fn serialize_body<B: Serialize + ?Sized>(body: &B) -> Result<String> {
    serde_json::to_string(body).context("serialise request body")
}

fn map_error(status: StatusCode, body: &str) -> anyhow::Error {
    // Best-effort: surface server error codes when present.
    if let Ok(env) = serde_json::from_str::<ErrorEnvelope>(body) {
        let code = env.code.as_deref().unwrap_or("");
        let msg = env.message.as_deref().unwrap_or("");
        return anyhow!(
            "dashboard error {} {}: {}{}",
            status.as_u16(),
            code,
            msg,
            if status == StatusCode::UNAUTHORIZED {
                " — run `shardd auth login`"
            } else {
                ""
            }
        );
    }
    anyhow!(
        "dashboard error {}: {}{}",
        status.as_u16(),
        body,
        if status == StatusCode::UNAUTHORIZED {
            " — run `shardd auth login`"
        } else {
            ""
        }
    )
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}
