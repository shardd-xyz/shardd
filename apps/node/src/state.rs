//! Core state manager per protocol.md v1.7 §3-5.
//!
//! SharedState holds all in-memory caches (§5) and implements event
//! creation (§3.1) and replication (§3.2) with per-account atomic sections.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use dashmap::DashMap;
use shardd_types::*;

/// Per-account state held under a Mutex for the atomic section (§3.1).
#[derive(Debug)]
pub struct AccountState {
    pub balance: i64,
    pub event_count: usize,
    /// Active holds: (event_id, hold_amount, hold_expires_at_unix_ms)
    pub holds: Vec<(String, u64, u64)>,
    /// Event IDs whose holds have been released via hold_release events
    pub released: HashSet<String>,
}

impl AccountState {
    fn new() -> Self {
        Self { balance: 0, event_count: 0, holds: Vec::new(), released: HashSet::new() }
    }

    /// Compute available_balance = balance - active_holds (§11.3).
    pub fn available_balance(&self, now_ms: u64) -> i64 {
        let active_holds: i64 = self.holds.iter()
            .filter(|(eid, _, expires)| *expires > now_ms && !self.released.contains(eid))
            .map(|(_, amount, _)| *amount as i64)
            .sum();
        self.balance - active_holds
    }
}

/// All node state. Generic over storage backend for testability.
#[derive(Clone)]
pub struct SharedState<S: shardd_storage::StorageBackend> {
    pub node_id: Arc<str>,
    pub addr: Arc<str>,
    pub current_epoch: u32,
    pub next_seq: Arc<std::sync::atomic::AtomicU64>,
    pub storage: Arc<S>,

    /// Per-account state under Mutex for atomic section (§3.1).
    accounts: Arc<DashMap<BalanceKey, Arc<Mutex<AccountState>>>>,
    /// Contiguous head per (origin, epoch) (§2.5).
    heads: Arc<DashMap<EpochKey, u64>>,
    /// Out-of-order sequences per (origin, epoch) for head advancement (§5.1).
    pending_seqs: Arc<DashMap<EpochKey, BTreeSet<u64>>>,
    /// Account → set of (origin, epoch) pairs that contributed events.
    account_origin_epochs: Arc<DashMap<BalanceKey, HashSet<EpochKey>>>,
    /// Max known sequence per (origin, epoch).
    max_known_seqs: Arc<DashMap<EpochKey, u64>>,
    /// Full events for orphan recovery + serving recent events.
    event_buffer: Arc<DashMap<OriginKey, Event>>,
    /// Tracks what's not yet in Postgres: key → created_at_ms.
    unpersisted: Arc<DashMap<OriginKey, u64>>,
    /// Idempotency cache: (nonce, bucket, account, amount) → winning Event.
    idempotency_cache: Arc<DashMap<(String, String, String, i64), Event>>,

    /// Channel to send events to BatchWriter.
    pub batch_tx: mpsc::UnboundedSender<Event>,

    pub total_event_count: Arc<AtomicUsize>,

    /// Hold configuration (§11.4).
    pub hold_multiplier: u64,
    pub hold_duration_ms: u64,

    /// Node phase for readiness gate (§13.2).
    pub phase: Arc<std::sync::atomic::AtomicU8>,

    /// Channel for correction events (voids/hold_releases) to be broadcast.
    correction_tx: mpsc::UnboundedSender<Event>,
}

impl<S: shardd_storage::StorageBackend> SharedState<S> {
    /// Build a new SharedState. Rebuilds caches from storage on init.
    pub async fn new(
        node_id: String,
        addr: String,
        current_epoch: u32,
        storage: S,
        batch_tx: mpsc::UnboundedSender<Event>,
        correction_tx: mpsc::UnboundedSender<Event>,
        hold_multiplier: u64,
        hold_duration_ms: u64,
    ) -> Self {
        let storage = Arc::new(storage);
        let accounts: DashMap<BalanceKey, Arc<Mutex<AccountState>>> = DashMap::new();
        let heads: DashMap<EpochKey, u64> = DashMap::new();
        let account_origin_epochs: DashMap<BalanceKey, HashSet<EpochKey>> = DashMap::new();
        let max_known_seqs: DashMap<EpochKey, u64> = DashMap::new();
        let mut total_events = 0usize;

        // Rebuild balances from storage
        if let Ok(balances) = storage.aggregate_balances().await {
            for (bucket, account, sum) in balances {
                let key = (bucket, account);
                accounts.insert(key, Arc::new(Mutex::new(AccountState {
                    balance: sum, event_count: 0, holds: Vec::new(), released: HashSet::new(),
                })));
            }
        }

        // Rebuild heads + max_known from storage
        if let Ok(seqs_by_epoch) = storage.sequences_by_origin_epoch().await {
            for (epoch_key, seqs) in &seqs_by_epoch {
                total_events += seqs.len();
                let head = compute_contiguous_head(seqs);
                heads.insert(epoch_key.clone(), head);
                if let Some(&max) = seqs.last() {
                    max_known_seqs.insert(epoch_key.clone(), max);
                }
            }
        }

        // Rebuild origin→account mapping
        if let Ok(mapping) = storage.origin_account_epoch_mapping().await {
            for (origin, epoch, bucket, account) in mapping {
                account_origin_epochs
                    .entry((bucket, account))
                    .or_default()
                    .insert((origin, epoch));
            }
        }

        // Rebuild holds from storage
        let now_ms = Event::now_ms();
        if let Ok(hold_events) = storage.active_holds(now_ms).await {
            for event in &hold_events {
                let key = event.balance_key();
                let acct = accounts.entry(key).or_insert_with(|| Arc::new(Mutex::new(AccountState::new())));
                let mut state = acct.lock().await;
                state.holds.push((event.event_id.clone(), event.hold_amount, event.hold_expires_at_unix_ms));
            }
        }
        if let Ok(released) = storage.released_hold_refs().await {
            // Mark released holds across all accounts
            for ref_id in released {
                for entry in accounts.iter() {
                    let mut state = entry.value().lock().await;
                    if state.holds.iter().any(|(eid, _, _)| eid == &ref_id) {
                        state.released.insert(ref_id.clone());
                    }
                }
            }
        }

        // §10.3: Rebuild idempotency cache from recent nonce events in DB
        let idempotency_cache: DashMap<(String, String, String, i64), Event> = DashMap::new();
        if let Ok(all_events) = storage.query_all_events_sorted().await {
            for event in all_events.iter().rev().take(10000) { // last 10K events
                if let Some(ref nonce) = event.idempotency_nonce {
                    let key = (nonce.clone(), event.bucket.clone(), event.account.clone(), event.amount);
                    idempotency_cache.entry(key).or_insert_with(|| event.clone());
                }
            }
        }

        let next_seq = storage.derive_next_seq(&node_id, current_epoch).await.unwrap_or(1);

        Self {
            node_id: Arc::from(node_id.as_str()),
            addr: Arc::from(addr.as_str()),
            current_epoch,
            next_seq: Arc::new(std::sync::atomic::AtomicU64::new(next_seq)),
            storage,
            accounts: Arc::new(accounts),
            heads: Arc::new(heads),
            pending_seqs: Arc::new(DashMap::new()),
            account_origin_epochs: Arc::new(account_origin_epochs),
            max_known_seqs: Arc::new(max_known_seqs),
            event_buffer: Arc::new(DashMap::new()),
            unpersisted: Arc::new(DashMap::new()),
            idempotency_cache: Arc::new(idempotency_cache),
            batch_tx,
            correction_tx,
            total_event_count: Arc::new(AtomicUsize::new(total_events)),
            hold_multiplier,
            hold_duration_ms,
            phase: Arc::new(std::sync::atomic::AtomicU8::new(0)), // 0 = Warming
        }
    }

    // ── Event creation (§3.1) ────────────────────────────────────────

    /// Create a local event within the per-account atomic section.
    /// Returns (event, ack_placeholder) or error.
    pub async fn create_local_event(
        &self,
        bucket: String,
        account: String,
        amount: i64,
        note: Option<String>,
        max_overdraft: u64,
        idempotency_nonce: Option<String>,
    ) -> Result<Event, (i64, i64, i64)> {
        // §11.4: Compute hold metadata for debits
        let (hold_amount, hold_expires_at_unix_ms) = if amount < 0 && self.hold_multiplier > 0 {
            let ha = (amount.unsigned_abs()) * self.hold_multiplier;
            let exp = Event::now_ms() + self.hold_duration_ms;
            (ha, exp)
        } else {
            (0, 0)
        };
        let key = (bucket.clone(), account.clone());
        let acct = self.accounts.entry(key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(AccountState::new())))
            .clone();

        // Hold the per-account lock for the entire atomic section (§3.1)
        let mut state = acct.lock().await;

        // Step 1: Idempotency check (§10.3)
        // Check in-memory cache first, then DB fallback.
        if let Some(ref nonce) = idempotency_nonce {
            let idem_key = (nonce.clone(), bucket.clone(), account.clone(), amount);

            // In-memory cache hit
            if let Some(existing) = self.idempotency_cache.get(&idem_key) {
                return Ok(existing.value().clone()); // deduplicated
            }

            // DB fallback (§10.3 step 2): cache may have been evicted after restart
            if let Ok(matches) = self.storage.find_by_idempotency_key(nonce, &bucket, &account, amount).await {
                if !matches.is_empty() {
                    // Determine canonical winner per §10.4
                    let winner = matches.iter().min_by(|a, b| {
                        a.created_at_unix_ms.cmp(&b.created_at_unix_ms)
                            .then_with(|| a.event_id.cmp(&b.event_id))
                    }).unwrap().clone();

                    // Re-populate cache for future fast lookups
                    self.idempotency_cache.insert(idem_key, winner.clone());
                    return Ok(winner); // deduplicated
                }
            }
        }

        // Step 2: Overdraft guard (§9.1) — debits only
        let now_ms = Event::now_ms();
        if amount < 0 {
            let avail = state.available_balance(now_ms);
            let projected = avail + amount - (hold_amount as i64);
            let floor = -(max_overdraft as i64);
            if projected < floor {
                return Err((state.balance, avail, projected));
            }
        }

        // Step 3: Assign sequence
        let seq = self.next_seq.fetch_add(1, Relaxed);

        // Step 4: Generate event
        let event = Event {
            event_id: Event::generate_id(),
            origin_node_id: self.node_id.to_string(),
            origin_epoch: self.current_epoch,
            origin_seq: seq,
            created_at_unix_ms: now_ms,
            r#type: EventType::Standard,
            bucket: bucket.clone(),
            account: account.clone(),
            amount,
            note,
            idempotency_nonce: idempotency_nonce.clone(),
            void_ref: None,
            hold_amount,
            hold_expires_at_unix_ms,
        };

        // Step 5: Update in-memory caches (still holding lock)
        state.balance += amount;
        state.event_count += 1;
        if event.has_hold() {
            state.holds.push((event.event_id.clone(), hold_amount, hold_expires_at_unix_ms));
        }

        // Install in idempotency cache
        if let Some(ref nonce) = idempotency_nonce {
            self.idempotency_cache.insert(
                (nonce.clone(), bucket.clone(), account.clone(), amount),
                event.clone(),
            );
        }

        // Release per-account lock
        drop(state);

        // Update non-account caches
        self.advance_head(&event.epoch_key(), event.origin_seq);
        self.update_origin_tracking(&event);
        self.store_event_buffer(&event);
        self.total_event_count.fetch_add(1, Relaxed);

        // Queue for async persistence
        let _ = self.batch_tx.send(event.clone());

        Ok(event)
    }

    // ── Event replication (§3.2) ─────────────────────────────────────

    /// Insert a replicated event. Returns true if newly inserted.
    pub async fn insert_event(&self, event: &Event) -> bool {
        let key = event.origin_key();

        // Entry-level dedup on event_buffer (prevents concurrent double-apply)
        if self.event_buffer.contains_key(&key) {
            return false;
        }

        // Check head-based dedup
        let head = self.heads.get(&event.epoch_key()).map(|v| *v).unwrap_or(0);
        if event.origin_seq <= head {
            return false;
        }

        // Insert into event_buffer atomically (entry API holds shard lock)
        use dashmap::mapref::entry::Entry;
        match self.event_buffer.entry(key.clone()) {
            Entry::Occupied(_) => return false, // another thread got there first
            Entry::Vacant(v) => { v.insert(event.clone()); }
        }

        // Update account state
        let acct_key = event.balance_key();
        let acct = self.accounts.entry(acct_key)
            .or_insert_with(|| Arc::new(Mutex::new(AccountState::new())))
            .clone();

        // Use try_lock to avoid deadlock (replicated events don't need the full atomic section)
        // If lock is held by a local create, the balance update is safe because
        // replicated events don't check overdraft.
        let mut state = acct.lock().await;
        state.balance += event.amount;
        state.event_count += 1;

        // Track holds from replicated events
        if event.has_hold() {
            state.holds.push((event.event_id.clone(), event.hold_amount, event.hold_expires_at_unix_ms));
        }
        if event.r#type == EventType::HoldRelease {
            if let Some(ref void_ref) = event.void_ref {
                state.released.insert(void_ref.clone());
            }
        }
        drop(state);

        // Update non-account caches
        self.advance_head(&event.epoch_key(), event.origin_seq);
        self.update_origin_tracking(event);
        self.unpersisted.insert(key, event.created_at_unix_ms);
        self.total_event_count.fetch_add(1, Relaxed);

        // Queue for async persistence to this node's own PG
        let _ = self.batch_tx.send(event.clone());

        // §10.4: Cross-node idempotency conflict check
        self.check_idempotency_conflict(event).await;

        true
    }

    /// Check for idempotency conflicts and emit corrections (§10.4-10.6).
    async fn check_idempotency_conflict(&self, event: &Event) {
        let nonce = match &event.idempotency_nonce {
            Some(n) => n.clone(),
            None => return,
        };

        let idem_key = (nonce.clone(), event.bucket.clone(), event.account.clone(), event.amount);

        // Check if we already have an event with this idempotency key
        let existing = self.idempotency_cache.get(&idem_key).map(|e| e.value().clone());
        let existing = match existing {
            Some(e) if e.event_id != event.event_id => Some(e),
            _ => None,
        };

        let existing = match existing {
            Some(e) => e,
            None => {
                // No conflict — install in cache
                self.idempotency_cache.insert(idem_key, event.clone());
                return;
            }
        };

        // Conflict! Determine winner per §10.4
        let (winner, loser) = shardd_types::idempotency_winner(&existing, event);

        // Update cache to point to winner
        self.idempotency_cache.insert(idem_key, winner.clone());

        // §10.5: Emit void for the loser (if not already emitted)
        let void_nonce = format!("void:{}", loser.event_id);
        let void_idem = (void_nonce.clone(), loser.bucket.clone(), loser.account.clone(), -loser.amount);

        if self.idempotency_cache.contains_key(&void_idem) {
            return; // Already emitted (or another node did)
        }

        // Check DB too
        if let Ok(matches) = self.storage.find_by_idempotency_key(&void_nonce, &loser.bucket, &loser.account, -loser.amount).await {
            if !matches.is_empty() { return; }
        }

        // Emit void event
        let seq = self.next_seq.fetch_add(1, Relaxed);
        let void_event = Event {
            event_id: Event::generate_id(),
            origin_node_id: self.node_id.to_string(),
            origin_epoch: self.current_epoch,
            origin_seq: seq,
            created_at_unix_ms: Event::now_ms(),
            r#type: EventType::Void,
            bucket: loser.bucket.clone(),
            account: loser.account.clone(),
            amount: -loser.amount,
            note: Some(format!("void: duplicate of event {}", winner.event_id)),
            idempotency_nonce: Some(void_nonce.clone()),
            void_ref: Some(loser.event_id.clone()),
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };

        // Apply void to state
        self.apply_correction_event(&void_event).await;

        // §10.5 step 2: If loser had a hold, emit hold_release
        if loser.has_hold() {
            let release_nonce = format!("release:{}", loser.event_id);
            let release_idem = (release_nonce.clone(), loser.bucket.clone(), loser.account.clone(), 0);

            if !self.idempotency_cache.contains_key(&release_idem) {
                let release_seq = self.next_seq.fetch_add(1, Relaxed);
                let release_event = Event {
                    event_id: Event::generate_id(),
                    origin_node_id: self.node_id.to_string(),
                    origin_epoch: self.current_epoch,
                    origin_seq: release_seq,
                    created_at_unix_ms: Event::now_ms(),
                    r#type: EventType::HoldRelease,
                    bucket: loser.bucket.clone(),
                    account: loser.account.clone(),
                    amount: 0,
                    note: Some(format!("release hold: duplicate of event {}", winner.event_id)),
                    idempotency_nonce: Some(release_nonce),
                    void_ref: Some(loser.event_id.clone()),
                    hold_amount: 0,
                    hold_expires_at_unix_ms: 0,
                };
                self.apply_correction_event(&release_event).await;
            }
        }
    }

    /// Apply a correction event (void or hold_release) to local state.
    async fn apply_correction_event(&self, event: &Event) {
        let key = event.origin_key();
        self.event_buffer.insert(key.clone(), event.clone());
        self.unpersisted.insert(key, event.created_at_unix_ms);

        let acct_key = event.balance_key();
        let acct = self.accounts.entry(acct_key)
            .or_insert_with(|| Arc::new(Mutex::new(AccountState::new())))
            .clone();
        let mut state = acct.lock().await;
        state.balance += event.amount;
        state.event_count += 1;
        if event.r#type == EventType::HoldRelease {
            if let Some(ref void_ref) = event.void_ref {
                state.released.insert(void_ref.clone());
            }
        }
        drop(state);

        self.advance_head(&event.epoch_key(), event.origin_seq);
        self.update_origin_tracking(event);
        self.total_event_count.fetch_add(1, Relaxed);

        // Install correction in idempotency cache
        if let Some(ref nonce) = event.idempotency_nonce {
            self.idempotency_cache.insert(
                (nonce.clone(), event.bucket.clone(), event.account.clone(), event.amount),
                event.clone(),
            );
        }

        let _ = self.batch_tx.send(event.clone());
        // §4.1: Broadcast correction events to peers
        let _ = self.correction_tx.send(event.clone());
    }

    /// Insert a batch of events. Returns count of newly inserted.
    pub async fn insert_events_batch(&self, events: &[Event]) -> usize {
        let mut count = 0;
        for event in events {
            if self.insert_event(event).await { count += 1; }
        }
        count
    }

    // ── Reads (in-memory) ────────────────────────────────────────────

    pub fn total_balance(&self) -> i64 {
        self.accounts.iter()
            .map(|e| {
                // Use try_lock to avoid blocking
                e.value().try_lock()
                    .map(|s| s.balance)
                    .unwrap_or(0)
            })
            .sum()
    }

    pub fn account_balance(&self, bucket: &str, account: &str) -> i64 {
        self.accounts.get(&(bucket.to_string(), account.to_string()))
            .and_then(|a| a.try_lock().ok().map(|s| s.balance))
            .unwrap_or(0)
    }

    pub fn account_available_balance(&self, bucket: &str, account: &str) -> i64 {
        let now_ms = Event::now_ms();
        self.accounts.get(&(bucket.to_string(), account.to_string()))
            .and_then(|a| a.try_lock().ok().map(|s| s.available_balance(now_ms)))
            .unwrap_or(0)
    }

    pub fn get_all_balances(&self) -> Vec<AccountBalance> {
        let now_ms = Event::now_ms();
        let mut result = Vec::new();
        for entry in self.accounts.iter() {
            let (bucket, account) = entry.key();
            if let Ok(s) = entry.value().try_lock() {
                result.push(AccountBalance {
                    bucket: bucket.clone(),
                    account: account.clone(),
                    balance: s.balance,
                    available_balance: s.available_balance(now_ms),
                    active_hold_total: s.holds.iter()
                        .filter(|(eid, _, exp)| *exp > now_ms && !s.released.contains(eid))
                        .map(|(_, amt, _)| *amt as i64)
                        .sum(),
                    event_count: s.event_count,
                });
            }
        }
        result.sort_by(|a, b| a.bucket.cmp(&b.bucket).then_with(|| a.account.cmp(&b.account)));
        result
    }

    pub fn get_heads(&self) -> BTreeMap<String, u64> {
        self.heads.iter()
            .map(|e| {
                let (origin, epoch) = e.key();
                (format!("{origin}:{epoch}"), *e.value())
            })
            .collect()
    }

    pub fn event_count(&self) -> usize {
        self.total_event_count.load(Relaxed)
    }

    /// Sweep expired holds from all accounts (§5.3).
    /// Call periodically from a background task.
    pub async fn sweep_expired_holds(&self) {
        let now_ms = Event::now_ms();
        for entry in self.accounts.iter() {
            if let Ok(mut state) = entry.value().try_lock() {
                // Collect expired hold event_ids
                let expired: Vec<String> = state.holds.iter()
                    .filter(|(_, _, expires)| *expires <= now_ms)
                    .map(|(eid, _, _)| eid.clone())
                    .collect();

                // Remove expired holds
                state.holds.retain(|(_, _, expires)| *expires > now_ms);

                // Evict release markers for expired holds
                for eid in &expired {
                    state.released.remove(eid);
                }
            }
        }
    }

    pub fn get_events_from_buffer(&self, origin: &str, epoch: u32, from_seq: u64, to_seq: u64) -> Vec<Event> {
        (from_seq..=to_seq)
            .filter_map(|seq| {
                self.event_buffer.get(&(origin.to_string(), epoch, seq))
                    .map(|e| e.value().clone())
            })
            .collect()
    }

    // ── Persistence tracking ─────────────────────────────────────────

    pub fn mark_persisted(&self, keys: &[(String, u32, u64)]) {
        for key in keys {
            self.unpersisted.remove(key);
        }
    }

    pub fn get_unpersisted_events(&self, cutoff_ms: u64) -> Vec<Event> {
        self.unpersisted.iter()
            .filter(|e| *e.value() <= cutoff_ms)
            .filter_map(|e| self.event_buffer.get(e.key()).map(|v| v.value().clone()))
            .collect()
    }

    pub fn persistence_stats(&self) -> PersistenceStats {
        let now = Event::now_ms();
        let oldest = self.unpersisted.iter()
            .map(|e| *e.value())
            .min()
            .map(|ts| now.saturating_sub(ts));
        PersistenceStats {
            buffered: self.event_buffer.len(),
            unpersisted: self.unpersisted.len(),
            oldest_unpersisted_age_ms: oldest,
        }
    }

    // ── Collapsed state (§2.6) ───────────────────────────────────────

    pub fn collapsed_state(&self) -> BTreeMap<String, CollapsedBalance> {
        let now_ms = Event::now_ms();
        let mut result = BTreeMap::new();

        for entry in self.accounts.iter() {
            let (bucket, account) = entry.key();
            let key = format!("{bucket}:{account}");

            let state = match entry.value().try_lock() {
                Ok(s) => s,
                Err(_) => continue,
            };

            let mut origins = BTreeMap::new();
            if let Some(epoch_set) = self.account_origin_epochs.get(&(bucket.clone(), account.clone())) {
                for (origin, epoch) in epoch_set.iter() {
                    let head = self.heads.get(&(origin.clone(), *epoch)).map(|v| *v).unwrap_or(0);
                    let max_known = self.max_known_seqs.get(&(origin.clone(), *epoch)).map(|v| *v).unwrap_or(0);
                    origins.insert(format!("{origin}:{epoch}"), OriginProgress { head, max_known });
                }
            }

            let status = if origins.is_empty() || origins.values().all(|o| o.head >= o.max_known) {
                "locally_confirmed".to_string()
            } else {
                "provisional".to_string()
            };

            result.insert(key, CollapsedBalance {
                balance: state.balance,
                available_balance: state.available_balance(now_ms),
                status,
                contributing_origins: origins,
            });
        }
        result
    }

    // ── Debug (§7.1) ──────────────────────────────────────────────────

    pub fn debug_origin(&self, origin_id: &str) -> DebugOriginResponse {
        let mut epochs = BTreeMap::new();
        // Find all epoch keys for this origin
        for entry in self.heads.iter() {
            let (origin, epoch) = entry.key();
            if origin != origin_id { continue; }
            let head = *entry.value();
            let pending: Vec<u64> = self.pending_seqs
                .get(&(origin.clone(), *epoch))
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default();
            let max_known = self.max_known_seqs
                .get(&(origin.clone(), *epoch))
                .map(|v| *v)
                .unwrap_or(head);
            // present_seqs = 1..=head + pending
            let mut present: Vec<u64> = (1..=head).collect();
            present.extend(&pending);
            let count = present.len();
            epochs.insert(*epoch, DebugEpochInfo {
                contiguous_head: head,
                present_seqs: pending, // only gaps/pending, not all seqs (too large)
                min_seq: if count > 0 { Some(1) } else { None },
                max_seq: if max_known > 0 { Some(max_known) } else { None },
                count,
            });
        }
        DebugOriginResponse {
            origin_node_id: origin_id.to_string(),
            epochs,
        }
    }

    // ── Private helpers ──────────────────────────────────────────────

    fn advance_head(&self, epoch_key: &EpochKey, seq: u64) {
        let current = self.heads.get(epoch_key).map(|v| *v).unwrap_or(0);
        if seq == current + 1 {
            let new_head = self.drain_pending(epoch_key, seq);
            self.heads.insert(epoch_key.clone(), new_head);
        } else if seq > current + 1 {
            self.pending_seqs.entry(epoch_key.clone()).or_default().insert(seq);
            self.heads.entry(epoch_key.clone()).or_insert(current);
        }
    }

    fn drain_pending(&self, epoch_key: &EpochKey, current_head: u64) -> u64 {
        let mut head = current_head;
        if let Some(mut pending) = self.pending_seqs.get_mut(epoch_key) {
            while pending.contains(&(head + 1)) {
                pending.remove(&(head + 1));
                head += 1;
            }
        }
        head
    }

    fn update_origin_tracking(&self, event: &Event) {
        self.account_origin_epochs
            .entry(event.balance_key())
            .or_default()
            .insert(event.epoch_key());
        self.max_known_seqs
            .entry(event.epoch_key())
            .and_modify(|max| { if event.origin_seq > *max { *max = event.origin_seq; } })
            .or_insert(event.origin_seq);
    }

    fn store_event_buffer(&self, event: &Event) {
        let key = event.origin_key();
        self.event_buffer.insert(key.clone(), event.clone());
        self.unpersisted.insert(key, event.created_at_unix_ms);
    }
}

// ── UnpersistedSource impl for OrphanDetector ────────────────────────

impl<S: shardd_storage::StorageBackend> crate::orphan_detector::UnpersistedSource for SharedState<S> {
    fn get_unpersisted_events(&self, cutoff_ms: u64) -> Vec<Event> {
        self.unpersisted.iter()
            .filter(|e| *e.value() <= cutoff_ms)
            .filter_map(|e| self.event_buffer.get(e.key()).map(|v| v.value().clone()))
            .collect()
    }

    fn mark_persisted(&self, keys: &[(String, u32, u64)]) {
        for key in keys {
            self.unpersisted.remove(key);
        }
    }
}

fn compute_contiguous_head(seqs: &[u64]) -> u64 {
    let mut head = 0u64;
    for &seq in seqs {
        if seq == head + 1 { head = seq; } else if seq > head + 1 { break; }
    }
    head
}

#[cfg(test)]
mod tests {
    use super::*;
    use shardd_storage::StorageBackend;
    use shardd_storage::memory::InMemoryStorage;

    async fn make_state() -> SharedState<InMemoryStorage> {
        let storage = InMemoryStorage::new();
        storage.save_node_meta(&NodeMeta {
            node_id: "test-node".into(), host: "127.0.0.1".into(), port: 0,
            current_epoch: 1, next_seq: 1,
        }).await.unwrap();

        let (tx, _rx) = mpsc::unbounded_channel();
        let (ctx, _crx) = mpsc::unbounded_channel();
        SharedState::new("test-node".into(), "127.0.0.1:3000".into(), 1, storage, tx, ctx, 0, 0).await
    }

    async fn make_state_with_holds() -> SharedState<InMemoryStorage> {
        let storage = InMemoryStorage::new();
        storage.save_node_meta(&NodeMeta {
            node_id: "test-node".into(), host: "127.0.0.1".into(), port: 0,
            current_epoch: 1, next_seq: 1,
        }).await.unwrap();

        let (tx, _rx) = mpsc::unbounded_channel();
        let (ctx, _crx) = mpsc::unbounded_channel();
        // hold_multiplier=5, hold_duration=600000ms (10 min)
        SharedState::new("test-node".into(), "127.0.0.1:3000".into(), 1, storage, tx, ctx, 5, 600_000).await
    }

    #[tokio::test]
    async fn create_event_increments_seq() {
        let state = make_state().await;
        let e1 = state.create_local_event("b".into(), "a".into(), 100, None, 0, None).await.unwrap();
        let e2 = state.create_local_event("b".into(), "a".into(), 50, None, 0, None).await.unwrap();
        assert_eq!(e1.origin_seq, 1);
        assert_eq!(e2.origin_seq, 2);
        assert_eq!(e1.origin_epoch, 1);
    }

    #[tokio::test]
    async fn overdraft_guard_rejects() {
        let state = make_state().await;
        state.create_local_event("b".into(), "a".into(), 100, None, 0, None).await.unwrap();
        let result = state.create_local_event("b".into(), "a".into(), -200, None, 0, None).await;
        assert!(result.is_err());
        assert_eq!(state.account_balance("b", "a"), 100); // unchanged
    }

    #[tokio::test]
    async fn overdraft_guard_with_limit() {
        let state = make_state().await;
        state.create_local_event("b".into(), "a".into(), 100, None, 0, None).await.unwrap();
        let result = state.create_local_event("b".into(), "a".into(), -200, None, 200, None).await;
        assert!(result.is_ok());
        assert_eq!(state.account_balance("b", "a"), -100);
    }

    #[tokio::test]
    async fn replicated_event_bypass_overdraft() {
        let state = make_state().await;
        let event = Event {
            event_id: "remote-1".into(), origin_node_id: "remote".into(),
            origin_epoch: 1, origin_seq: 1, created_at_unix_ms: 1000,
            r#type: EventType::Standard, bucket: "b".into(), account: "a".into(),
            amount: -999, note: None, idempotency_nonce: None, void_ref: None,
            hold_amount: 0, hold_expires_at_unix_ms: 0,
        };
        assert!(state.insert_event(&event).await);
        assert_eq!(state.account_balance("b", "a"), -999);
    }

    #[tokio::test]
    async fn replication_dedup() {
        let state = make_state().await;
        let event = Event {
            event_id: "e1".into(), origin_node_id: "n1".into(),
            origin_epoch: 1, origin_seq: 1, created_at_unix_ms: 1000,
            r#type: EventType::Standard, bucket: "b".into(), account: "a".into(),
            amount: 100, note: None, idempotency_nonce: None, void_ref: None,
            hold_amount: 0, hold_expires_at_unix_ms: 0,
        };
        assert!(state.insert_event(&event).await);
        assert!(!state.insert_event(&event).await); // duplicate
        assert_eq!(state.account_balance("b", "a"), 100); // not 200
    }

    #[tokio::test]
    async fn head_advancement_with_gap_fill() {
        let state = make_state().await;
        let make = |seq: u64| Event {
            event_id: format!("e{seq}"), origin_node_id: "n1".into(),
            origin_epoch: 1, origin_seq: seq, created_at_unix_ms: seq * 1000,
            r#type: EventType::Standard, bucket: "b".into(), account: "a".into(),
            amount: 1, note: None, idempotency_nonce: None, void_ref: None,
            hold_amount: 0, hold_expires_at_unix_ms: 0,
        };

        state.insert_event(&make(1)).await;
        state.insert_event(&make(3)).await; // gap at 2
        let heads = state.get_heads();
        assert_eq!(heads.get("n1:1"), Some(&1)); // stuck at 1

        state.insert_event(&make(2)).await; // fill gap
        let heads = state.get_heads();
        assert_eq!(heads.get("n1:1"), Some(&3)); // advanced to 3
    }

    #[tokio::test]
    async fn epoch_aware_heads() {
        let state = make_state().await;
        let make = |epoch: u32, seq: u64| Event {
            event_id: format!("e{epoch}-{seq}"), origin_node_id: "n1".into(),
            origin_epoch: epoch, origin_seq: seq, created_at_unix_ms: 1000,
            r#type: EventType::Standard, bucket: "b".into(), account: "a".into(),
            amount: 1, note: None, idempotency_nonce: None, void_ref: None,
            hold_amount: 0, hold_expires_at_unix_ms: 0,
        };

        state.insert_event(&make(1, 1)).await;
        state.insert_event(&make(1, 2)).await;
        state.insert_event(&make(2, 1)).await; // different epoch

        let heads = state.get_heads();
        assert_eq!(heads.get("n1:1"), Some(&2));
        assert_eq!(heads.get("n1:2"), Some(&1));
    }

    #[tokio::test]
    async fn idempotency_local_dedup() {
        let state = make_state().await;
        state.create_local_event("b".into(), "a".into(), 100, None, 0, None).await.unwrap();

        let e1 = state.create_local_event("b".into(), "a".into(), -50, None, 0, Some("nonce1".into())).await.unwrap();
        let e2 = state.create_local_event("b".into(), "a".into(), -50, None, 0, Some("nonce1".into())).await.unwrap();

        assert_eq!(e1.event_id, e2.event_id); // same event returned
        assert_eq!(state.account_balance("b", "a"), 50); // charged once, not twice
    }

    #[tokio::test]
    async fn available_balance_with_holds() {
        // Use state with hold_multiplier=5, so a -100 debit creates a 500 hold
        let state = make_state_with_holds().await;
        state.create_local_event("b".into(), "a".into(), 1000, None, 0, None).await.unwrap();
        state.create_local_event("b".into(), "a".into(), -100, None, 0, None).await.unwrap();

        assert_eq!(state.account_balance("b", "a"), 900); // settled
        assert_eq!(state.account_available_balance("b", "a"), 400); // 900 - 500 hold (100 * 5)
    }

    #[tokio::test]
    async fn collapsed_state_confirmed_vs_provisional() {
        let state = make_state().await;
        state.create_local_event("b".into(), "a".into(), 100, None, 0, None).await.unwrap();

        let collapsed = state.collapsed_state();
        assert_eq!(collapsed["b:a"].status, "locally_confirmed");

        // Add remote events with a gap
        let make = |seq: u64| Event {
            event_id: format!("r{seq}"), origin_node_id: "remote".into(),
            origin_epoch: 1, origin_seq: seq, created_at_unix_ms: 1000,
            r#type: EventType::Standard, bucket: "b".into(), account: "a".into(),
            amount: 10, note: None, idempotency_nonce: None, void_ref: None,
            hold_amount: 0, hold_expires_at_unix_ms: 0,
        };
        state.insert_event(&make(1)).await;
        state.insert_event(&make(3)).await; // gap at 2

        let collapsed = state.collapsed_state();
        assert_eq!(collapsed["b:a"].status, "provisional");
    }

    #[tokio::test]
    async fn persistence_tracking() {
        let state = make_state().await;
        assert_eq!(state.persistence_stats().unpersisted, 0);

        let event = Event {
            event_id: "e1".into(), origin_node_id: "n1".into(),
            origin_epoch: 1, origin_seq: 1, created_at_unix_ms: 1000,
            r#type: EventType::Standard, bucket: "b".into(), account: "a".into(),
            amount: 100, note: None, idempotency_nonce: None, void_ref: None,
            hold_amount: 0, hold_expires_at_unix_ms: 0,
        };
        state.insert_event(&event).await;

        assert_eq!(state.persistence_stats().unpersisted, 1);
        state.mark_persisted(&[("n1".into(), 1, 1)]);
        assert_eq!(state.persistence_stats().unpersisted, 0);
    }

    #[tokio::test]
    async fn idempotency_db_fallback_after_cache_miss() {
        // Create a state, insert event with nonce, then manually write it to storage
        // and clear the in-memory cache to simulate a restart scenario
        let storage = InMemoryStorage::new();
        storage.save_node_meta(&NodeMeta {
            node_id: "test-node".into(), host: "127.0.0.1".into(), port: 0,
            current_epoch: 1, next_seq: 1,
        }).await.unwrap();

        // Pre-insert an event with a nonce directly into storage (simulating prior run)
        let prior_event = Event {
            event_id: "prior-event-123".into(),
            origin_node_id: "test-node".into(),
            origin_epoch: 1,
            origin_seq: 1,
            created_at_unix_ms: 1000,
            r#type: EventType::Standard,
            bucket: "b".into(),
            account: "a".into(),
            amount: -50,
            note: None,
            idempotency_nonce: Some("nonce-db-test".into()),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };
        storage.insert_event(&prior_event).await.unwrap();

        // Create state — idempotency cache is empty (not rebuilt from DB)
        let (tx, _rx) = mpsc::unbounded_channel();
        let (ctx, _crx) = mpsc::unbounded_channel();
        let state = SharedState::new("test-node".into(), "127.0.0.1:3000".into(), 1, storage, tx, ctx, 0, 0).await;

        // Fund the account so overdraft doesn't reject
        state.create_local_event("b".into(), "a".into(), 1000, None, 0, None).await.unwrap();

        // Now try to create with the same nonce — should hit DB fallback and dedup
        let result = state.create_local_event("b".into(), "a".into(), -50, None, 0, Some("nonce-db-test".into())).await.unwrap();

        // Should return the prior event (dedup from DB)
        assert_eq!(result.event_id, "prior-event-123");
        // Balance should NOT have been charged again
        assert_eq!(state.account_balance("b", "a"), 950); // 1000 - 50 from rebuild, not -50 again
    }

    #[tokio::test]
    async fn cross_node_idempotency_conflict_emits_void() {
        let state = make_state().await;
        state.create_local_event("b".into(), "a".into(), 1000, None, 0, None).await.unwrap();

        // This node creates an event with a nonce
        let local = state.create_local_event("b".into(), "a".into(), -50, None, 0, Some("completion:abc".into())).await.unwrap();

        // A remote node also created an event with the same nonce (older timestamp)
        let remote = Event {
            event_id: "remote-event-older".into(),
            origin_node_id: "remote-node".into(),
            origin_epoch: 1,
            origin_seq: 1,
            created_at_unix_ms: local.created_at_unix_ms - 1000, // older = winner
            r#type: EventType::Standard,
            bucket: "b".into(),
            account: "a".into(),
            amount: -50,
            note: None,
            idempotency_nonce: Some("completion:abc".into()),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };

        // Insert remote event — should trigger conflict detection
        state.insert_event(&remote).await;

        // Balance should be: 1000 - 50 (remote wins) - 50 (local) + 50 (void of local)
        // = 1000 - 50 = 950
        assert_eq!(state.account_balance("b", "a"), 950);

        // Verify event count: 1 credit + 1 local debit + 1 remote debit + 1 void = 4
        assert!(state.event_count() >= 4);
    }

    #[tokio::test]
    async fn idempotency_different_amount_same_nonce_not_dedup() {
        let state = make_state().await;
        state.create_local_event("b".into(), "a".into(), 1000, None, 0, None).await.unwrap();

        let e1 = state.create_local_event("b".into(), "a".into(), -50, None, 0, Some("nonce1".into())).await.unwrap();
        let e2 = state.create_local_event("b".into(), "a".into(), -100, None, 0, Some("nonce1".into())).await.unwrap();

        // Different amounts = different operations, not duplicates
        assert_ne!(e1.event_id, e2.event_id);
        assert_eq!(state.account_balance("b", "a"), 850); // 1000 - 50 - 100
    }
}
