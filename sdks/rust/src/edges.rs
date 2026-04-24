use futures::future::join_all;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::error::ShardError;
use crate::types::{EdgeDirectoryResponse, EdgeHealth, EdgeInfo};

/// Prod bootstrap list. The SDK probes these three regions on first use
/// and then refreshes the list from `/gateway/edges`.
pub(crate) const DEFAULT_EDGES: &[&str] = &[
    "https://use1.api.shardd.xyz",
    "https://euc1.api.shardd.xyz",
    "https://ape1.api.shardd.xyz",
];

/// Sync-gap above which we treat an edge as too stale to pick.
const MAX_ACCEPTABLE_SYNC_GAP: u64 = 100;

/// How long a failed edge stays on the bench before we re-try it.
const COOLDOWN_MS: u64 = 60_000;

/// Parallel probe timeout per edge.
const PROBE_TIMEOUT_MS: u64 = 2_000;

#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub base_url: String,
    pub rtt_ms: Option<u64>,
    pub cooldown_until: Option<Instant>,
}

impl Candidate {
    fn new(base_url: String) -> Self {
        Self {
            base_url,
            rtt_ms: None,
            cooldown_until: None,
        }
    }

    fn is_cool(&self, now: Instant) -> bool {
        self.cooldown_until.map(|t| t > now).unwrap_or(false)
    }
}

/// Edge selector state — parallel-probes on first use, keeps a ranked
/// list in memory, and opens cooldown windows on 5xx/timeout/network
/// errors. Thread-safe: wrapped in a Mutex so concurrent requests on
/// the same Client share the same selection.
pub(crate) struct EdgeSelector {
    inner: Mutex<Inner>,
}

struct Inner {
    /// Ranked candidates — index 0 is the current pick.
    candidates: Vec<Candidate>,
    /// Whether we've done at least one full probe since construction.
    initialized: bool,
}

impl EdgeSelector {
    pub(crate) fn new(bootstrap: Vec<String>) -> Self {
        let candidates = bootstrap.into_iter().map(Candidate::new).collect();
        Self {
            inner: Mutex::new(Inner {
                candidates,
                initialized: false,
            }),
        }
    }

    /// Snapshot the currently-ranked base URLs that aren't in cooldown.
    /// Callers iterate this; if empty, refresh via [`probe_all`](Self::probe_all).
    pub(crate) fn live_urls(&self) -> Vec<String> {
        let now = Instant::now();
        let guard = self.inner.lock().unwrap();
        guard
            .candidates
            .iter()
            .filter(|c| !c.is_cool(now))
            .map(|c| c.base_url.clone())
            .collect()
    }

    pub(crate) fn needs_probe(&self) -> bool {
        let now = Instant::now();
        let guard = self.inner.lock().unwrap();
        if !guard.initialized {
            return true;
        }
        // All candidates cool → re-probe to let a recovered cluster back in.
        !guard.candidates.iter().any(|c| !c.is_cool(now))
    }

    pub(crate) fn mark_failure(&self, base_url: &str) {
        let mut guard = self.inner.lock().unwrap();
        let until = Instant::now() + Duration::from_millis(COOLDOWN_MS);
        for c in &mut guard.candidates {
            if c.base_url == base_url {
                c.cooldown_until = Some(until);
            }
        }
    }

    pub(crate) fn mark_success(&self, base_url: &str) {
        let mut guard = self.inner.lock().unwrap();
        for c in &mut guard.candidates {
            if c.base_url == base_url {
                c.cooldown_until = None;
            }
        }
    }

    /// Replace the candidate list with the gateway's view — keeps RTT
    /// data for URLs we already measured. Call after a successful
    /// request discovers `/gateway/edges`.
    #[allow(dead_code)]
    pub(crate) fn replace_directory(&self, fresh: Vec<EdgeInfo>) {
        let mut guard = self.inner.lock().unwrap();
        let existing: std::collections::HashMap<String, Candidate> = guard
            .candidates
            .drain(..)
            .map(|c| (c.base_url.clone(), c))
            .collect();
        for edge in fresh {
            let base_url = edge.base_url;
            let c = existing
                .get(&base_url)
                .cloned()
                .unwrap_or_else(|| Candidate::new(base_url.clone()));
            guard.candidates.push(c);
        }
        if guard.candidates.is_empty() {
            // fallback: don't wipe ourselves out if the directory came back empty
            guard.candidates = existing.into_values().collect();
        }
    }

    /// Probe every candidate in parallel and re-rank by measured RTT.
    /// A probe **is a weak signal**: a transient 0-healthy-nodes blip
    /// at the gateway (it happens ~once per 5s while the mesh client
    /// refreshes) should NOT cool the edge for 60s — the next probe
    /// will usually see it healthy again, and cooling would starve
    /// future requests. So probes only re-rank; real-request failures
    /// open cooldowns.
    pub(crate) async fn probe_all(&self, http: &reqwest::Client) -> Result<(), ShardError> {
        let urls: Vec<String> = {
            let guard = self.inner.lock().unwrap();
            guard
                .candidates
                .iter()
                .map(|c| c.base_url.clone())
                .collect()
        };
        if urls.is_empty() {
            return Err(ShardError::ServiceUnavailable("no edges configured".into()));
        }
        let probes = urls.iter().map(|url| probe_one(http, url.clone()));
        let results = join_all(probes).await;

        let now = Instant::now();
        let mut guard = self.inner.lock().unwrap();
        guard.initialized = true;
        for (i, result) in results.into_iter().enumerate() {
            let c = &mut guard.candidates[i];
            match result {
                Ok((rtt_ms, health)) if is_selectable(&health) => {
                    c.rtt_ms = Some(rtt_ms);
                    // Successful probe clears any prior request-level
                    // cooldown; the edge is observed healthy now.
                    c.cooldown_until = None;
                }
                _ => {
                    c.rtt_ms = None;
                    // Do NOT open a cooldown here. See doc above.
                }
            }
        }
        // Sort: already-cool last, then ascending RTT. Candidates with
        // no RTT (probe failed or not yet measured) come after those
        // with a measured RTT.
        guard
            .candidates
            .sort_by(|a, b| match (a.is_cool(now), b.is_cool(now)) {
                (false, true) => std::cmp::Ordering::Less,
                (true, false) => std::cmp::Ordering::Greater,
                _ => a
                    .rtt_ms
                    .unwrap_or(u64::MAX)
                    .cmp(&b.rtt_ms.unwrap_or(u64::MAX)),
            });
        Ok(())
    }
}

async fn probe_one(http: &reqwest::Client, base_url: String) -> Result<(u64, EdgeHealth), ()> {
    let url = format!("{}/gateway/health", base_url.trim_end_matches('/'));
    let start = Instant::now();
    let resp = tokio::time::timeout(
        Duration::from_millis(PROBE_TIMEOUT_MS),
        http.get(&url).send(),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    if !resp.status().is_success() {
        return Err(());
    }
    let health: EdgeHealth = resp.json().await.map_err(|_| ())?;
    let rtt_ms = start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    Ok((rtt_ms, health))
}

fn is_selectable(health: &EdgeHealth) -> bool {
    if !health.ready {
        return false;
    }
    if matches!(health.overloaded, Some(true)) {
        return false;
    }
    if health
        .sync_gap
        .map(|g| g > MAX_ACCEPTABLE_SYNC_GAP)
        .unwrap_or(false)
    {
        return false;
    }
    true
}

pub(crate) async fn fetch_directory(
    http: &reqwest::Client,
    base_url: &str,
) -> Result<EdgeDirectoryResponse, ShardError> {
    let url = format!("{}/gateway/edges", base_url.trim_end_matches('/'));
    let resp = http
        .get(&url)
        .send()
        .await
        .map_err(|e| ShardError::Network(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(ShardError::ServiceUnavailable(format!(
            "edges fetch returned HTTP {}",
            resp.status().as_u16()
        )));
    }
    resp.json()
        .await
        .map_err(|e| ShardError::Decode(e.to_string()))
}
