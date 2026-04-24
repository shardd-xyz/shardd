use std::sync::Arc;
use std::time::Duration;

use reqwest::{Method, Response, StatusCode};
use serde::de::DeserializeOwned;

use crate::edges::{fetch_directory, EdgeSelector, DEFAULT_EDGES};
use crate::error::{from_status, GatewayErrorBody, ShardError};
use crate::types::{
    AccountDetail, Balances, CreateEventBody, CreateEventOptions, CreateEventResult, EdgeHealth,
    EdgeInfo, Event, EventList,
};

/// Builder for a [`Client`]. Use this to override the default prod
/// bootstrap list, plug in a custom `reqwest::Client`, or change the
/// request timeout.
///
/// ```no_run
/// use shardd::Client;
///
/// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
/// let client = Client::builder()
///     .api_key("sk_live_...".to_string())
///     .build()?;
/// # Ok(())
/// # }
/// ```
pub struct ClientBuilder {
    api_key: Option<String>,
    edges: Option<Vec<String>>,
    timeout_ms: u64,
    http: Option<reqwest::Client>,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self {
            api_key: None,
            edges: None,
            timeout_ms: 30_000,
            http: None,
        }
    }
}

impl ClientBuilder {
    pub fn api_key(mut self, api_key: String) -> Self {
        self.api_key = Some(api_key);
        self
    }

    /// Override the edge bootstrap list — useful for local testing
    /// against the docker harness or a self-hosted cluster.
    pub fn edges(mut self, edges: Vec<String>) -> Self {
        self.edges = Some(edges);
        self
    }

    pub fn timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }

    pub fn http(mut self, http: reqwest::Client) -> Self {
        self.http = Some(http);
        self
    }

    pub fn build(self) -> Result<Client, ShardError> {
        let api_key = self
            .api_key
            .ok_or_else(|| ShardError::InvalidInput("api_key is required".into()))?;
        let bootstrap = self
            .edges
            .unwrap_or_else(|| DEFAULT_EDGES.iter().map(|s| s.to_string()).collect());
        let http = self.http.unwrap_or_else(|| {
            reqwest::Client::builder()
                .timeout(Duration::from_millis(self.timeout_ms))
                .build()
                .expect("reqwest client build")
        });
        Ok(Client {
            inner: Arc::new(ClientInner {
                api_key,
                http,
                selector: EdgeSelector::new(bootstrap),
            }),
        })
    }
}

/// Thread-safe handle to the shardd API. Cloning is cheap (`Arc` bump).
///
/// ```no_run
/// use shardd::Client;
///
/// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
/// let client = Client::new("sk_live_...".to_string())?;
/// let result = client
///     .create_event("my-app", "user:42", -100, Default::default())
///     .await?;
/// println!("charged, new balance = {}", result.balance);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Client {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    api_key: String,
    http: reqwest::Client,
    selector: EdgeSelector,
}

impl Client {
    /// Shorthand for `Client::builder().api_key(...).build()`.
    pub fn new(api_key: String) -> Result<Self, ShardError> {
        Self::builder().api_key(api_key).build()
    }

    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// Create a ledger event. Positive `amount` = credit, negative = debit.
    /// The SDK auto-generates a UUID v4 for `idempotency_nonce` unless
    /// you supply your own via [`CreateEventOptions`].
    pub async fn create_event(
        &self,
        bucket: &str,
        account: &str,
        amount: i64,
        opts: CreateEventOptions,
    ) -> Result<CreateEventResult, ShardError> {
        let nonce = opts
            .idempotency_nonce
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let body = CreateEventBody {
            bucket,
            account,
            amount,
            note: opts.note.as_deref(),
            idempotency_nonce: &nonce,
            max_overdraft: opts.max_overdraft,
            min_acks: opts.min_acks,
            ack_timeout_ms: opts.ack_timeout_ms,
            hold_amount: opts.hold_amount,
            hold_expires_at_unix_ms: opts.hold_expires_at_unix_ms,
        };
        self.request_json(Method::POST, "/events", Some(&body), None::<&()>)
            .await
    }

    /// List events in a bucket, newest first, capped by the server
    /// (typically the last ~500 events). For filtering beyond bucket,
    /// use the gateway's richer filters via a raw HTTP call.
    pub async fn list_events(&self, bucket: &str) -> Result<EventList, ShardError> {
        self.request_json::<_, _, EventList>(
            Method::GET,
            "/events",
            None::<&()>,
            Some(&[("bucket", bucket)]),
        )
        .await
    }

    /// Read every account balance within a bucket.
    pub async fn get_balances(&self, bucket: &str) -> Result<Balances, ShardError> {
        self.request_json::<_, _, Balances>(
            Method::GET,
            "/balances",
            None::<&()>,
            Some(&[("bucket", bucket)]),
        )
        .await
    }

    /// Single-account snapshot: balance, available balance (after
    /// holds), active hold total, event count.
    pub async fn get_account(
        &self,
        bucket: &str,
        account: &str,
    ) -> Result<AccountDetail, ShardError> {
        let path = format!("/collapsed/{}/{}", urlencoded(bucket), urlencoded(account));
        self.request_json::<(), (), AccountDetail>(Method::GET, &path, None, None)
            .await
    }

    /// Discover the current edge directory. The SDK calls this
    /// transparently on first use; this method is for observability
    /// and for building your own region-aware routing on top.
    pub async fn edges(&self) -> Result<Vec<EdgeInfo>, ShardError> {
        self.ensure_probed().await?;
        let live = self.inner.selector.live_urls();
        let Some(base) = live.first() else {
            return Err(ShardError::ServiceUnavailable("no healthy edges".into()));
        };
        let dir = fetch_directory(&self.inner.http, base).await?;
        Ok(dir.edges)
    }

    /// Health of a specific edge, or the currently-pinned edge if
    /// `base_url` is `None`.
    pub async fn health(&self, base_url: Option<&str>) -> Result<EdgeHealth, ShardError> {
        let target = match base_url {
            Some(b) => b.to_string(),
            None => {
                self.ensure_probed().await?;
                self.inner
                    .selector
                    .live_urls()
                    .first()
                    .cloned()
                    .ok_or_else(|| ShardError::ServiceUnavailable("no healthy edges".into()))?
            }
        };
        let url = format!("{}/gateway/health", target.trim_end_matches('/'));
        let resp = self
            .inner
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| ShardError::Network(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(from_status(resp.status().as_u16(), None));
        }
        resp.json()
            .await
            .map_err(|e| ShardError::Decode(e.to_string()))
    }

    // ── internal plumbing ───────────────────────────────────────────

    async fn ensure_probed(&self) -> Result<(), ShardError> {
        if self.inner.selector.needs_probe() {
            self.inner.selector.probe_all(&self.inner.http).await?;
        }
        Ok(())
    }

    async fn request_json<B, Q, R>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        query: Option<&Q>,
    ) -> Result<R, ShardError>
    where
        B: serde::Serialize + ?Sized,
        Q: serde::Serialize + ?Sized,
        R: DeserializeOwned,
    {
        self.ensure_probed().await?;

        let urls = self.inner.selector.live_urls();
        if urls.is_empty() {
            // Every edge cool — force a re-probe.
            self.inner.selector.probe_all(&self.inner.http).await?;
        }
        let urls = self.inner.selector.live_urls();
        if urls.is_empty() {
            return Err(ShardError::ServiceUnavailable("all edges unhealthy".into()));
        }

        // Try candidates in priority order, capped at 3. The cap
        // prevents an unhealthy multi-region rollout from ballooning
        // a single request into a large fan-out; 3 matches our
        // current prod topology (use1/euc1/ape1).
        let mut last_err: Option<ShardError> = None;
        for base in urls.iter().take(3) {
            let url = format!("{}{}", base.trim_end_matches('/'), path);
            let mut req = self
                .inner
                .http
                .request(method.clone(), &url)
                .bearer_auth(&self.inner.api_key);
            if let Some(b) = body {
                req = req.json(b);
            }
            if let Some(q) = query {
                req = req.query(q);
            }
            match req.send().await {
                Ok(resp) => match handle_response::<R>(resp).await {
                    Ok(value) => {
                        self.inner.selector.mark_success(base);
                        return Ok(value);
                    }
                    Err(err) if err.is_retryable() => {
                        self.inner.selector.mark_failure(base);
                        last_err = Some(err);
                    }
                    Err(err) => return Err(err),
                },
                Err(e) => {
                    self.inner.selector.mark_failure(base);
                    if e.is_timeout() {
                        last_err = Some(ShardError::RequestTimeout);
                    } else {
                        last_err = Some(ShardError::Network(e.to_string()));
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            ShardError::ServiceUnavailable("failover exhausted with no error captured".into())
        }))
    }
}

/// Extension method on [`Client`] for the most common case — call
/// `create_event` and unwrap `result.event`.
impl Client {
    pub async fn charge(
        &self,
        bucket: &str,
        account: &str,
        amount: u64,
        note: Option<&str>,
    ) -> Result<Event, ShardError> {
        let result = self
            .create_event(
                bucket,
                account,
                -(amount as i64),
                CreateEventOptions {
                    note: note.map(String::from),
                    ..Default::default()
                },
            )
            .await?;
        Ok(result.event)
    }

    pub async fn credit(
        &self,
        bucket: &str,
        account: &str,
        amount: u64,
        note: Option<&str>,
    ) -> Result<Event, ShardError> {
        let result = self
            .create_event(
                bucket,
                account,
                amount as i64,
                CreateEventOptions {
                    note: note.map(String::from),
                    ..Default::default()
                },
            )
            .await?;
        Ok(result.event)
    }
}

async fn handle_response<R: DeserializeOwned>(resp: Response) -> Result<R, ShardError> {
    let status = resp.status();
    if status.is_success() {
        return resp
            .json::<R>()
            .await
            .map_err(|e| ShardError::Decode(e.to_string()));
    }
    let code = status.as_u16();
    let body_bytes = resp.bytes().await.unwrap_or_default();
    let err_body: Option<GatewayErrorBody> = if body_bytes.is_empty() {
        None
    } else {
        serde_json::from_slice(&body_bytes).ok()
    };
    match status {
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => {
            Err(ShardError::RequestTimeout)
        }
        _ => Err(from_status(code, err_body)),
    }
}

fn urlencoded(s: &str) -> String {
    // Minimal inline encoder — enough for bucket/account names which are
    // short ASCII identifiers. Avoids pulling in urlencoding just for this.
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}
