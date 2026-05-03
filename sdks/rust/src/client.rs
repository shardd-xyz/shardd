use std::sync::Arc;
use std::time::Duration;

use reqwest::{Method, Response, StatusCode};
use serde::de::DeserializeOwned;

use crate::edges::{fetch_directory, EdgeSelector, DEFAULT_EDGES};
use crate::error::{from_status, GatewayErrorBody, ShardError};
use crate::types::{
    AccountDetail, Balances, BucketDeleteMode, CreateEventBody, CreateEventOptions,
    CreateEventResult, CreateMyEventBody, DeleteBucketResult, DeletedBucketsList, EdgeHealth,
    EdgeInfo, Event, EventList, MyBucketDetail, MyBucketEventsList, MyBucketsList, MyEventsList,
    Reservation,
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
                selector: Arc::new(EdgeSelector::new(bootstrap)),
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
    selector: Arc<EdgeSelector>,
}

impl Client {
    /// Shorthand for `Client::builder().api_key(...).build()`.
    pub fn new(api_key: String) -> Result<Self, ShardError> {
        Self::builder().api_key(api_key).build()
    }

    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// Clone this client while replacing only the bearer token. The
    /// HTTP pool and edge selector are shared, so callers can mint
    /// short-lived tokens without losing failover state.
    pub fn with_api_key(&self, api_key: String) -> Self {
        Self {
            inner: Arc::new(ClientInner {
                api_key,
                http: self.inner.http.clone(),
                selector: self.inner.selector.clone(),
            }),
        }
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
            settle_reservation: opts.settle_reservation.as_deref(),
            release_reservation: opts.release_reservation.as_deref(),
            skip_hold: opts.skip_hold,
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

    pub async fn list_my_buckets(
        &self,
        page: Option<usize>,
        limit: Option<usize>,
        q: Option<&str>,
    ) -> Result<MyBucketsList, ShardError> {
        #[derive(serde::Serialize)]
        struct Query<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            page: Option<usize>,
            #[serde(skip_serializing_if = "Option::is_none")]
            limit: Option<usize>,
            #[serde(skip_serializing_if = "Option::is_none")]
            q: Option<&'a str>,
        }
        self.request_json(
            Method::GET,
            "/v1/me/buckets",
            None::<&()>,
            Some(&Query { page, limit, q }),
        )
        .await
    }

    pub async fn list_my_deleted_buckets(&self) -> Result<DeletedBucketsList, ShardError> {
        self.request_json(
            Method::GET,
            "/v1/me/buckets/deleted",
            None::<&()>,
            None::<&()>,
        )
        .await
    }

    pub async fn get_my_bucket(&self, bucket: &str) -> Result<MyBucketDetail, ShardError> {
        let path = format!("/v1/me/buckets/{}", urlencoded(bucket));
        self.request_json(Method::GET, &path, None::<&()>, None::<&()>)
            .await
    }

    pub async fn list_my_bucket_events(
        &self,
        bucket: &str,
        q: Option<&str>,
        account: Option<&str>,
        page: Option<usize>,
        limit: Option<usize>,
    ) -> Result<MyBucketEventsList, ShardError> {
        #[derive(serde::Serialize)]
        struct Query<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            q: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            account: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            page: Option<usize>,
            #[serde(skip_serializing_if = "Option::is_none")]
            limit: Option<usize>,
        }
        let path = format!("/v1/me/buckets/{}/events", urlencoded(bucket));
        self.request_json(
            Method::GET,
            &path,
            None::<&()>,
            Some(&Query {
                q,
                account,
                page,
                limit,
            }),
        )
        .await
    }

    pub async fn create_my_bucket_event(
        &self,
        bucket: &str,
        body: &CreateMyEventBody,
    ) -> Result<CreateEventResult, ShardError> {
        self.create_my_bucket_event_with_status(bucket, body)
            .await
            .map(|(_, value)| value)
    }

    pub async fn create_my_bucket_event_with_status(
        &self,
        bucket: &str,
        body: &CreateMyEventBody,
    ) -> Result<(u16, CreateEventResult), ShardError> {
        let path = format!("/v1/me/buckets/{}/events", urlencoded(bucket));
        self.request_json_with_status(Method::POST, &path, Some(body), None::<&()>)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn list_my_events(
        &self,
        bucket: Option<&str>,
        account: Option<&str>,
        origin: Option<&str>,
        event_type: Option<&str>,
        since_ms: Option<u64>,
        until_ms: Option<u64>,
        search: Option<&str>,
        limit: Option<u32>,
        offset: Option<u32>,
        replication: Option<bool>,
    ) -> Result<MyEventsList, ShardError> {
        #[derive(serde::Serialize)]
        struct Query<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            bucket: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            account: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            origin: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            event_type: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            since_ms: Option<u64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            until_ms: Option<u64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            search: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            limit: Option<u32>,
            #[serde(skip_serializing_if = "Option::is_none")]
            offset: Option<u32>,
            #[serde(skip_serializing_if = "Option::is_none")]
            replication: Option<bool>,
        }
        self.request_json(
            Method::GET,
            "/v1/me/events",
            None::<&()>,
            Some(&Query {
                bucket,
                account,
                origin,
                event_type,
                since_ms,
                until_ms,
                search,
                limit,
                offset,
                replication,
            }),
        )
        .await
    }

    pub async fn delete_my_bucket(
        &self,
        bucket: &str,
        mode: BucketDeleteMode,
    ) -> Result<DeleteBucketResult, ShardError> {
        #[derive(serde::Serialize)]
        struct Query<'a> {
            mode: &'a str,
        }
        let path = format!("/v1/me/buckets/{}", urlencoded(bucket));
        self.request_json(
            Method::DELETE,
            &path,
            None::<&()>,
            Some(&Query {
                mode: mode.as_str(),
            }),
        )
        .await
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
        self.request_json_with_status(method, path, body, query)
            .await
            .map(|(_, value)| value)
    }

    async fn request_json_with_status<B, Q, R>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        query: Option<&Q>,
    ) -> Result<(u16, R), ShardError>
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

    /// Reserve `amount` credit units for `ttl_ms`. Returns a
    /// [`Reservation`] handle whose `reservation_id` you pass to
    /// [`Client::settle`] (one-shot capture) or [`Client::release`]
    /// (cancel). If neither is called before `ttl_ms` elapses, the hold
    /// auto-releases passively and `available_balance` recovers.
    pub async fn reserve(
        &self,
        bucket: &str,
        account: &str,
        amount: u64,
        ttl_ms: u64,
        opts: CreateEventOptions,
    ) -> Result<Reservation, ShardError> {
        if amount == 0 {
            return Err(ShardError::InvalidInput(
                "reserve amount must be > 0".into(),
            ));
        }
        if ttl_ms == 0 {
            return Err(ShardError::InvalidInput(
                "reserve ttl_ms must be > 0".into(),
            ));
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let result = self
            .create_event(
                bucket,
                account,
                0,
                CreateEventOptions {
                    hold_amount: Some(amount),
                    hold_expires_at_unix_ms: Some(now_ms + ttl_ms),
                    ..opts
                },
            )
            .await?;
        Ok(Reservation {
            reservation_id: result.event.event_id.clone(),
            expires_at_unix_ms: result.event.hold_expires_at_unix_ms,
            balance: result.balance,
            available_balance: result.available_balance,
        })
    }

    /// Settle (one-shot capture) `amount` against an existing reservation.
    /// `amount` is the absolute value to charge; must be ≤ the
    /// reservation's hold. The server emits both the charge and a
    /// `hold_release`, returning any unused remainder to available balance.
    pub async fn settle(
        &self,
        bucket: &str,
        account: &str,
        reservation_id: &str,
        amount: u64,
        opts: CreateEventOptions,
    ) -> Result<CreateEventResult, ShardError> {
        self.create_event(
            bucket,
            account,
            -(amount as i64),
            CreateEventOptions {
                settle_reservation: Some(reservation_id.to_string()),
                ..opts
            },
        )
        .await
    }

    /// Cancel a reservation outright — releases the entire hold, no charge.
    pub async fn release(
        &self,
        bucket: &str,
        account: &str,
        reservation_id: &str,
        opts: CreateEventOptions,
    ) -> Result<CreateEventResult, ShardError> {
        self.create_event(
            bucket,
            account,
            0,
            CreateEventOptions {
                release_reservation: Some(reservation_id.to_string()),
                ..opts
            },
        )
        .await
    }
}

async fn handle_response<R: DeserializeOwned>(resp: Response) -> Result<(u16, R), ShardError> {
    let status = resp.status();
    let code = status.as_u16();
    if status.is_success() {
        return resp
            .json::<R>()
            .await
            .map(|body| (code, body))
            .map_err(|e| ShardError::Decode(e.to_string()));
    }
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
