use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use dashmap::DashMap;
use shardd_broadcast::{AckInfo, Broadcaster};
use shardd_storage::StorageBackend;
use shardd_types::{AccountBalance, BalanceKey, Event, NodeMeta};

use crate::peer::PeerSet;

// ── Account balance cache ────────────────────────────────────────────

struct AccountState {
    balance: AtomicI64,
    event_count: AtomicUsize,
}

// ── Collapsed state types ────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct OriginProgress { pub head: u64, pub max_known: u64 }

#[derive(Debug, Clone, serde::Serialize)]
pub struct CollapsedBalance {
    pub balance: i64,
    pub status: String,
    pub contributing_origins: BTreeMap<String, OriginProgress>,
}

pub type CollapsedState = BTreeMap<String, CollapsedBalance>;

// ── Persistence stats ────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct PersistenceStats {
    pub buffered: usize,
    pub unpersisted: usize,
    pub oldest_unpersisted_age_ms: Option<u64>,
}

// ── Trait for OrphanDetector to access state without knowing S ───────

pub trait SharedStateAny: Send + Sync {
    fn get_unpersisted_events(&self, cutoff_ms: u64) -> Vec<Event>;
    fn mark_persisted(&self, keys: &[(String, u64)]);
}

// ── SharedState ──────────────────────────────────────────────────────

#[derive(Clone)]
pub struct SharedState<S: StorageBackend> {
    pub node_id: Arc<str>,
    pub addr: Arc<str>,
    pub next_seq: Arc<AtomicU64>,
    pub peers: Arc<Mutex<PeerSet>>,
    pub storage: Arc<S>,
    /// Balance cache (hot path reads).
    accounts: Arc<DashMap<BalanceKey, AccountState>>,
    /// Contiguous head per origin.
    heads: Arc<DashMap<String, u64>>,
    /// Account → origins that contributed events.
    account_origins: Arc<DashMap<BalanceKey, HashSet<String>>>,
    /// Max known sequence per origin (for collapsed state).
    max_known_seqs: Arc<DashMap<String, u64>>,
    /// Full events for orphan recovery.
    event_buffer: Arc<DashMap<(String, u64), Event>>,
    /// Tracks what's not yet in Postgres: (origin, seq) → created_at_ms.
    unpersisted: Arc<DashMap<(String, u64), u64>>,
    /// Out-of-order sequences per origin (for in-memory head advancement).
    pending_seqs: Arc<DashMap<String, BTreeSet<u64>>>,
    /// Channel to send events to BatchWriter.
    batch_tx: mpsc::UnboundedSender<Event>,
    /// Broadcaster for cluster-wide event dissemination.
    broadcaster: Arc<dyn Broadcaster>,
    pub event_count: Arc<AtomicUsize>,
    pub total_balance: Arc<AtomicI64>,
}

impl<S: StorageBackend> SharedState<S> {
    pub async fn new(
        node_id: String,
        addr: String,
        next_seq: u64,
        peers: PeerSet,
        storage: S,
        batch_tx: mpsc::UnboundedSender<Event>,
        broadcaster: Arc<dyn Broadcaster>,
    ) -> Self {
        let storage = Arc::new(storage);
        let accounts: DashMap<BalanceKey, AccountState> = DashMap::new();
        let heads: DashMap<String, u64> = DashMap::new();
        let account_origins: DashMap<BalanceKey, HashSet<String>> = DashMap::new();
        let max_known_seqs: DashMap<String, u64> = DashMap::new();
        let mut total_events = 0usize;
        let mut total_balance = 0i64;

        // Rebuild caches from storage
        if let Ok(balances) = storage.aggregate_balances().await {
            for (bucket, account, sum) in balances {
                total_balance += sum;
                accounts.insert(
                    (bucket, account),
                    AccountState { balance: AtomicI64::new(sum), event_count: AtomicUsize::new(0) },
                );
            }
        }

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

        if let Ok(mapping) = storage.origin_account_mapping().await {
            for (origin, bucket, account) in mapping {
                account_origins.entry((bucket, account)).or_default().insert(origin);
            }
        }

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
            event_buffer: Arc::new(DashMap::new()),
            unpersisted: Arc::new(DashMap::new()),
            pending_seqs: Arc::new(DashMap::new()),
            batch_tx,
            broadcaster,
            event_count: Arc::new(AtomicUsize::new(total_events)),
            total_balance: Arc::new(AtomicI64::new(total_balance)),
        }
    }

    // ── Hot path: create local event (NO Postgres) ───────────────────

    pub async fn create_local_event(
        &self,
        bucket: String,
        account: String,
        amount: i64,
        note: Option<String>,
        max_overdraft: Option<u64>,
        min_acks: u32,
        ack_timeout_ms: u64,
    ) -> Result<(Event, AckInfo), (i64, i64)> {
        // Overdraft guard (atomic CAS, before any async work)
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
            bucket, account, amount, note,
        };

        // Update in-memory caches
        if !balance_pre_applied {
            self.track_account(&event);
        }
        self.event_count.fetch_add(1, Relaxed);
        self.total_balance.fetch_add(amount, Relaxed);
        self.update_origin_tracking(&event);
        self.advance_head(&event.origin_node_id, event.origin_seq);
        self.store_event_buffer(&event);

        // Queue for async Postgres write
        let _ = self.batch_tx.send(event.clone());

        // Broadcast to cluster (optionally wait for acks)
        let ack_info = self.broadcaster
            .broadcast_event(&event, min_acks, ack_timeout_ms)
            .await;

        Ok((event, ack_info))
    }

    // ── Replicated event (from broadcast or sync) ────────────────────

    pub fn insert_event(&self, event: Event) -> bool {
        let key = (event.origin_node_id.clone(), event.origin_seq);
        let head = self.heads.get(&event.origin_node_id).map(|v| *v).unwrap_or(0);

        // Dedup: already have it if seq <= head or in event_buffer
        if event.origin_seq <= head || self.event_buffer.contains_key(&key) {
            return false;
        }

        self.track_account(&event);
        self.event_count.fetch_add(1, Relaxed);
        self.total_balance.fetch_add(event.amount, Relaxed);
        self.update_origin_tracking(&event);
        self.advance_head(&event.origin_node_id, event.origin_seq);
        self.store_event_buffer(&event);

        // Queue for async Postgres write to THIS node's PG
        let _ = self.batch_tx.send(event);

        true
    }

    pub async fn insert_events_batch(&self, events: Vec<Event>) -> usize {
        let mut inserted = 0;
        for event in events {
            if self.insert_event(event) {
                inserted += 1;
            }
        }
        inserted
    }

    // ── Balance tracking ─────────────────────────────────────────────

    fn track_account(&self, event: &Event) {
        let key = (event.bucket.clone(), event.account.clone());
        let entry = self.accounts.entry(key).or_insert_with(|| AccountState {
            balance: AtomicI64::new(0), event_count: AtomicUsize::new(0),
        });
        entry.balance.fetch_add(event.amount, Relaxed);
        entry.event_count.fetch_add(1, Relaxed);
    }

    fn try_debit_account(&self, bucket: &str, account: &str, amount: i64, floor: i64) -> Result<i64, (i64, i64)> {
        let key = (bucket.to_string(), account.to_string());
        let entry = self.accounts.entry(key).or_insert_with(|| AccountState {
            balance: AtomicI64::new(0), event_count: AtomicUsize::new(0),
        });
        let result = entry.balance.fetch_update(Relaxed, Relaxed, |current| {
            let new = current + amount;
            if new >= floor { Some(new) } else { None }
        });
        match result {
            Ok(old) => { entry.event_count.fetch_add(1, Relaxed); Ok(old + amount) }
            Err(current) => Err((current, current + amount)),
        }
    }

    fn update_origin_tracking(&self, event: &Event) {
        self.account_origins
            .entry((event.bucket.clone(), event.account.clone()))
            .or_default()
            .insert(event.origin_node_id.clone());
        self.max_known_seqs
            .entry(event.origin_node_id.clone())
            .and_modify(|max| { if event.origin_seq > *max { *max = event.origin_seq; } })
            .or_insert(event.origin_seq);
    }

    fn store_event_buffer(&self, event: &Event) {
        let key = (event.origin_node_id.clone(), event.origin_seq);
        self.event_buffer.insert(key.clone(), event.clone());
        self.unpersisted.insert(key, event.created_at_unix_ms);
    }

    // ── In-memory head advancement ───────────────────────────────────

    fn advance_head(&self, origin_id: &str, seq: u64) {
        let current = self.heads.get(origin_id).map(|v| *v).unwrap_or(0);
        if seq == current + 1 {
            let new_head = self.drain_pending(origin_id, seq);
            self.heads.insert(origin_id.to_string(), new_head);
        } else if seq > current + 1 {
            self.pending_seqs.entry(origin_id.to_string()).or_default().insert(seq);
            self.heads.entry(origin_id.to_string()).or_insert(current);
        }
    }

    fn drain_pending(&self, origin_id: &str, current_head: u64) -> u64 {
        let mut head = current_head;
        if let Some(mut pending) = self.pending_seqs.get_mut(origin_id) {
            while pending.contains(&(head + 1)) {
                pending.remove(&(head + 1));
                head += 1;
            }
        }
        head
    }

    // ── Reads (in-memory) ────────────────────────────────────────────

    pub fn event_count(&self) -> usize { self.event_count.load(Relaxed) }
    pub fn total_balance(&self) -> i64 { self.total_balance.load(Relaxed) }

    pub fn account_balance(&self, bucket: &str, account: &str) -> i64 {
        self.accounts.get(&(bucket.to_string(), account.to_string()))
            .map(|e| e.balance.load(Relaxed)).unwrap_or(0)
    }

    pub fn all_balances(&self) -> Vec<AccountBalance> {
        let mut balances: Vec<AccountBalance> = self.accounts.iter().map(|entry| {
            let (bucket, account) = entry.key();
            AccountBalance {
                bucket: bucket.clone(), account: account.clone(),
                balance: entry.balance.load(Relaxed), event_count: entry.event_count.load(Relaxed),
            }
        }).collect();
        balances.sort_by(|a, b| a.bucket.cmp(&b.bucket).then_with(|| a.account.cmp(&b.account)));
        balances
    }

    pub fn get_heads(&self) -> BTreeMap<String, u64> {
        self.heads.iter().map(|e| (e.key().clone(), *e.value())).collect()
    }

    // ── Reads (storage-backed) ───────────────────────────────────────

    pub async fn get_events_range(&self, origin: &str, from_seq: u64, to_seq: u64) -> Vec<Event> {
        // Check event_buffer first (has recent unpersisted events), then fall back to storage
        let mut events: Vec<Event> = (from_seq..=to_seq)
            .filter_map(|seq| {
                self.event_buffer.get(&(origin.to_string(), seq)).map(|e| e.value().clone())
            })
            .collect();

        if events.len() < (to_seq - from_seq + 1) as usize {
            // Some events might only be in storage (persisted + cleared from buffer)
            let stored = self.storage.query_events_range(origin, from_seq, to_seq).await.unwrap_or_default();
            // Merge: prefer buffer (more recent), fill gaps from storage
            let buffered_seqs: std::collections::HashSet<u64> = events.iter().map(|e| e.origin_seq).collect();
            for e in stored {
                if !buffered_seqs.contains(&e.origin_seq) {
                    events.push(e);
                }
            }
            events.sort_by_key(|e| e.origin_seq);
        }

        events
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
            let status = if origins.is_empty() || origins.values().all(|o| o.head >= o.max_known) {
                "locally_confirmed".to_string()
            } else { "provisional".to_string() };
            result.insert(key, CollapsedBalance { balance, status, contributing_origins: origins });
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
        let status = if origins.is_empty() || origins.values().all(|o| o.head >= o.max_known) {
            "locally_confirmed".to_string()
        } else { "provisional".to_string() };
        CollapsedBalance { balance, status, contributing_origins: origins }
    }

    // ── Persistence tracking ─────────────────────────────────────────

    pub fn persistence_stats(&self) -> PersistenceStats {
        let buffered = self.event_buffer.len();
        let unpersisted = self.unpersisted.len();
        let now = now_ms();
        let oldest = self.unpersisted.iter()
            .map(|e| *e.value())
            .min()
            .map(|ts| now.saturating_sub(ts));
        PersistenceStats { buffered, unpersisted, oldest_unpersisted_age_ms: oldest }
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

// ── SharedStateAny impl (for OrphanDetector) ─────────────────────────

impl<S: StorageBackend> SharedStateAny for SharedState<S> {
    fn get_unpersisted_events(&self, cutoff_ms: u64) -> Vec<Event> {
        self.unpersisted.iter()
            .filter(|e| *e.value() <= cutoff_ms)
            .filter_map(|e| {
                let key = e.key().clone();
                self.event_buffer.get(&key).map(|v| v.value().clone())
            })
            .collect()
    }

    fn mark_persisted(&self, keys: &[(String, u64)]) {
        for (origin, seq) in keys {
            self.unpersisted.remove(&(origin.clone(), *seq));
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

fn compute_contiguous_head(seqs: &[u64]) -> u64 {
    let mut head = 0u64;
    for &seq in seqs {
        if seq == head + 1 { head = seq; } else if seq > head + 1 { break; }
    }
    head
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
