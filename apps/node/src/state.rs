use dashmap::DashMap;
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::Arc;
use tokio::sync::Mutex;

use shardd_storage::{InsertResult, StorageBackend};
use shardd_types::{AccountBalance, BalanceKey, Event, NodeMeta};

use crate::peer::PeerSet;

/// Per-account balance tracking (in-memory cache).
struct AccountState {
    balance: AtomicI64,
    event_count: AtomicUsize,
}

/// Collapsed state types for the API.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OriginProgress {
    pub head: u64,
    pub max_known: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CollapsedBalance {
    pub balance: i64,
    pub status: String,
    pub contributing_origins: BTreeMap<String, OriginProgress>,
}

pub type CollapsedState = BTreeMap<String, CollapsedBalance>;

// ── SharedState ──────────────────────────────────────────────────────

#[derive(Clone)]
pub struct SharedState<S: StorageBackend> {
    pub node_id: Arc<str>,
    pub addr: Arc<str>,
    pub next_seq: Arc<AtomicU64>,
    pub peers: Arc<Mutex<PeerSet>>,
    pub storage: Arc<S>,
    /// Per-(bucket, account) balance tracking (in-memory cache).
    accounts: Arc<DashMap<BalanceKey, AccountState>>,
    /// Contiguous head per origin (in-memory cache).
    heads: Arc<DashMap<String, u64>>,
    /// Account → set of origins that contributed events (for collapsed state).
    account_origins: Arc<DashMap<BalanceKey, HashSet<String>>>,
    /// Origin → max known sequence (cached to avoid N+1 queries).
    max_known_seqs: Arc<DashMap<String, u64>>,
    pub event_count: Arc<AtomicUsize>,
    pub total_balance: Arc<AtomicI64>,
}

impl<S: StorageBackend> SharedState<S> {
    /// Build a new SharedState, rebuilding caches from the storage backend.
    pub async fn new(
        node_id: String,
        addr: String,
        next_seq: u64,
        peers: PeerSet,
        storage: S,
    ) -> Self {
        let storage = Arc::new(storage);
        let accounts: DashMap<BalanceKey, AccountState> = DashMap::new();
        let heads: DashMap<String, u64> = DashMap::new();
        let account_origins: DashMap<BalanceKey, HashSet<String>> = DashMap::new();
        let max_known_seqs: DashMap<String, u64> = DashMap::new();
        let mut total_events = 0usize;
        let mut total_balance = 0i64;

        // Rebuild balance cache from storage
        if let Ok(balances) = storage.aggregate_balances().await {
            for (bucket, account, sum) in balances {
                total_balance += sum;
                let key = (bucket, account);
                accounts.insert(
                    key,
                    AccountState {
                        balance: AtomicI64::new(sum),
                        event_count: AtomicUsize::new(0),
                    },
                );
            }
        }

        // Rebuild heads + max_known from storage
        if let Ok(seqs_by_origin) = storage.sequences_by_origin().await {
            for (origin, seqs) in &seqs_by_origin {
                total_events += seqs.len();
                let head = compute_contiguous_head(seqs);
                heads.insert(origin.clone(), head);
                if let Some(&max) = seqs.last() {
                    max_known_seqs.insert(origin.clone(), max);
                }
            }
        }

        // Rebuild account→origins mapping
        if let Ok(mapping) = storage.origin_account_mapping().await {
            for (origin, bucket, account) in mapping {
                account_origins
                    .entry((bucket, account))
                    .or_default()
                    .insert(origin);
            }
        }

        // Crash safety: derive next_seq from max origin_seq
        let derived = storage.derive_next_seq(&node_id).await.unwrap_or(1);
        let safe_next_seq = next_seq.max(derived);

        Self {
            node_id: Arc::from(node_id.as_str()),
            addr: Arc::from(addr.as_str()),
            next_seq: Arc::new(AtomicU64::new(safe_next_seq)),
            peers: Arc::new(Mutex::new(peers)),
            storage,
            accounts: Arc::new(accounts),
            heads: Arc::new(heads),
            account_origins: Arc::new(account_origins),
            max_known_seqs: Arc::new(max_known_seqs),
            event_count: Arc::new(AtomicUsize::new(total_events)),
            total_balance: Arc::new(AtomicI64::new(total_balance)),
        }
    }

    // ── Balance tracking ─────────────────────────────────────────────

    fn track_account(&self, event: &Event) {
        let key = (event.bucket.clone(), event.account.clone());
        let entry = self.accounts.entry(key).or_insert_with(|| AccountState {
            balance: AtomicI64::new(0),
            event_count: AtomicUsize::new(0),
        });
        entry.balance.fetch_add(event.amount, Relaxed);
        entry.event_count.fetch_add(1, Relaxed);
    }

    fn rollback_balance(&self, event: &Event) {
        let key = (event.bucket.clone(), event.account.clone());
        if let Some(entry) = self.accounts.get(&key) {
            entry.balance.fetch_add(-event.amount, Relaxed);
            entry.event_count.fetch_sub(1, Relaxed);
        }
    }

    fn update_origin_tracking(&self, event: &Event) {
        // Update account→origins mapping
        self.account_origins
            .entry((event.bucket.clone(), event.account.clone()))
            .or_default()
            .insert(event.origin_node_id.clone());

        // Update max_known_seqs
        self.max_known_seqs
            .entry(event.origin_node_id.clone())
            .and_modify(|max| {
                if event.origin_seq > *max {
                    *max = event.origin_seq;
                }
            })
            .or_insert(event.origin_seq);
    }

    // ── Head advancement (DB-backed) ─────────────────────────────────

    async fn advance_head(&self, origin: &str, seq: u64) {
        let current = self.heads.get(origin).map(|v| *v).unwrap_or(0);

        if seq == current + 1 {
            // Next expected — query storage for consecutive sequences beyond
            let seqs = self
                .storage
                .sequences_from(origin, seq + 1)
                .await
                .unwrap_or_default();
            let mut head = seq;
            for s in seqs {
                if s == head + 1 {
                    head = s;
                } else {
                    break;
                }
            }
            self.heads.insert(origin.to_string(), head);
        } else if seq > current + 1 {
            // Gap — ensure entry exists but don't advance
            self.heads.entry(origin.to_string()).or_insert(current);
        }
    }

    // ── Overdraft guard ──────────────────────────────────────────────

    fn try_debit_account(
        &self,
        bucket: &str,
        account: &str,
        amount: i64,
        floor: i64,
    ) -> Result<i64, (i64, i64)> {
        let key = (bucket.to_string(), account.to_string());
        let entry = self.accounts.entry(key).or_insert_with(|| AccountState {
            balance: AtomicI64::new(0),
            event_count: AtomicUsize::new(0),
        });

        let result = entry.balance.fetch_update(Relaxed, Relaxed, |current| {
            let new = current + amount;
            if new >= floor { Some(new) } else { None }
        });

        match result {
            Ok(old) => {
                entry.event_count.fetch_add(1, Relaxed);
                Ok(old + amount)
            }
            Err(current) => Err((current, current + amount)),
        }
    }

    // ── Event insertion ──────────────────────────────────────────────

    /// Insert a replicated event. Returns true if newly inserted.
    pub async fn insert_event(&self, event: Event) -> bool {
        match self.storage.insert_event(&event).await {
            Ok(InsertResult::Inserted) => {
                self.track_account(&event);
                self.event_count.fetch_add(1, Relaxed);
                self.total_balance.fetch_add(event.amount, Relaxed);
                self.update_origin_tracking(&event);
                self.advance_head(&event.origin_node_id, event.origin_seq).await;
                true
            }
            Ok(InsertResult::Duplicate) => false,
            Ok(InsertResult::Conflict { details }) => {
                tracing::warn!("conflict: {details}");
                false
            }
            Err(e) => {
                tracing::warn!(error = %e, "insert_event failed");
                false
            }
        }
    }

    /// Insert a batch of replicated events. Returns count of newly inserted.
    pub async fn insert_events_batch(&self, events: Vec<Event>) -> usize {
        let mut inserted = 0usize;
        for event in events {
            if self.insert_event(event).await {
                inserted += 1;
            }
        }
        inserted
    }

    /// Create a local event with overdraft guard.
    pub async fn create_local_event(
        &self,
        bucket: String,
        account: String,
        amount: i64,
        note: Option<String>,
        max_overdraft: Option<u64>,
    ) -> Result<Event, (i64, i64)> {
        // Pre-apply balance atomically (before any async work)
        let balance_pre_applied = if amount < 0 {
            let floor = match max_overdraft {
                Some(v) => -(v.min(i64::MAX as u64) as i64),
                None => 0,
            };
            self.try_debit_account(&bucket, &account, amount, floor)?;
            true
        } else {
            false
        };

        let seq = self.next_seq.fetch_add(1, Relaxed);
        let event = Event {
            event_id: uuid::Uuid::new_v4().to_string(),
            origin_node_id: self.node_id.to_string(),
            origin_seq: seq,
            created_at_unix_ms: now_ms(),
            bucket,
            account,
            amount,
            note,
        };

        // Write to storage
        match self.storage.insert_event(&event).await {
            Ok(InsertResult::Inserted) => {
                // Credit: track the balance now (debits were pre-applied)
                if !balance_pre_applied {
                    self.track_account(&event);
                }
                self.event_count.fetch_add(1, Relaxed);
                self.total_balance.fetch_add(amount, Relaxed);
                self.update_origin_tracking(&event);
                self.advance_head(&event.origin_node_id, event.origin_seq).await;

                // Persist next_seq
                let _ = self.storage.save_node_meta(&NodeMeta {
                    node_id: self.node_id.to_string(),
                    host: self.addr.split(':').next().unwrap_or("127.0.0.1").to_string(),
                    port: self.addr.split(':').nth(1).and_then(|p| p.parse().ok()).unwrap_or(0),
                    next_seq: seq + 1,
                }).await;

                Ok(event)
            }
            _ => {
                // DB write failed — rollback pre-applied balance
                if balance_pre_applied {
                    self.rollback_balance(&event);
                }
                // This shouldn't happen for local events (unique event_id), but handle it
                tracing::error!("failed to persist local event {}", event.event_id);
                Err((0, 0))
            }
        }
    }

    // ── Reads (in-memory caches) ─────────────────────────────────────

    pub fn event_count(&self) -> usize {
        self.event_count.load(Relaxed)
    }

    pub fn total_balance(&self) -> i64 {
        self.total_balance.load(Relaxed)
    }

    pub fn account_balance(&self, bucket: &str, account: &str) -> i64 {
        self.accounts
            .get(&(bucket.to_string(), account.to_string()))
            .map(|e| e.balance.load(Relaxed))
            .unwrap_or(0)
    }

    pub fn all_balances(&self) -> Vec<AccountBalance> {
        let mut balances: Vec<AccountBalance> = self
            .accounts
            .iter()
            .map(|entry| {
                let (bucket, account) = entry.key();
                AccountBalance {
                    bucket: bucket.clone(),
                    account: account.clone(),
                    balance: entry.balance.load(Relaxed),
                    event_count: entry.event_count.load(Relaxed),
                }
            })
            .collect();
        balances.sort_by(|a, b| a.bucket.cmp(&b.bucket).then_with(|| a.account.cmp(&b.account)));
        balances
    }

    pub fn get_heads(&self) -> BTreeMap<String, u64> {
        self.heads.iter().map(|e| (e.key().clone(), *e.value())).collect()
    }

    // ── Reads (storage-backed, on-demand) ────────────────────────────

    pub async fn get_events_range(&self, origin: &str, from_seq: u64, to_seq: u64) -> Vec<Event> {
        self.storage.query_events_range(origin, from_seq, to_seq).await.unwrap_or_default()
    }

    pub async fn all_events_sorted(&self) -> Vec<Event> {
        self.storage.query_all_events_sorted().await.unwrap_or_default()
    }

    pub async fn checksum(&self) -> String {
        self.storage.checksum_data().await.unwrap_or_default()
    }

    pub async fn persist_peers(&self) {
        let addrs = self.peers.lock().await.to_vec();
        for addr in &addrs {
            let _ = self.storage.save_peer(addr).await;
        }
    }

    // ── Collapsed state ──────────────────────────────────────────────

    pub fn collapsed_state(&self) -> CollapsedState {
        let mut result = BTreeMap::new();

        for entry in self.accounts.iter() {
            let (bucket, account) = entry.key();
            let balance = entry.balance.load(Relaxed);
            let key = format!("{bucket}:{account}");

            let mut origins = BTreeMap::new();
            if let Some(origin_set) = self.account_origins.get(&(bucket.clone(), account.clone())) {
                for origin_id in origin_set.iter() {
                    let head = self.heads.get(origin_id).map(|v| *v).unwrap_or(0);
                    let max_known = self.max_known_seqs.get(origin_id).map(|v| *v).unwrap_or(0);
                    origins.insert(origin_id.clone(), OriginProgress { head, max_known });
                }
            }

            let status = if origins.is_empty()
                || origins.values().all(|o| o.head >= o.max_known)
            {
                "locally_confirmed".to_string()
            } else {
                "provisional".to_string()
            };

            result.insert(key, CollapsedBalance {
                balance,
                status,
                contributing_origins: origins,
            });
        }
        result
    }

    pub fn collapsed_balance(&self, bucket: &str, account: &str) -> CollapsedBalance {
        let balance = self.account_balance(bucket, account);
        let key = (bucket.to_string(), account.to_string());

        let mut origins = BTreeMap::new();
        if let Some(origin_set) = self.account_origins.get(&key) {
            for origin_id in origin_set.iter() {
                let head = self.heads.get(origin_id).map(|v| *v).unwrap_or(0);
                let max_known = self.max_known_seqs.get(origin_id).map(|v| *v).unwrap_or(0);
                origins.insert(origin_id.clone(), OriginProgress { head, max_known });
            }
        }

        let status = if origins.is_empty()
            || origins.values().all(|o| o.head >= o.max_known)
        {
            "locally_confirmed".to_string()
        } else {
            "provisional".to_string()
        };

        CollapsedBalance { balance, status, contributing_origins: origins }
    }

    // ── Debug ────────────────────────────────────────────────────────

    pub async fn debug_origin(&self, origin_id: &str) -> (u64, Vec<u64>, Option<u64>, Option<u64>, usize) {
        let head = self.heads.get(origin_id).map(|v| *v).unwrap_or(0);
        let seqs = self.storage.sequences_from(origin_id, 1).await.unwrap_or_default();
        let min = seqs.first().copied();
        let max = seqs.last().copied();
        let count = seqs.len();
        (head, seqs, min, max, count)
    }
}

fn compute_contiguous_head(seqs: &[u64]) -> u64 {
    let mut head = 0u64;
    for &seq in seqs {
        if seq == head + 1 {
            head = seq;
        } else if seq > head + 1 {
            break;
        }
    }
    head
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
