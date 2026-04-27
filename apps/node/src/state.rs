//! Core state manager per protocol.md v1.8 §3-5.
//!
//! SharedState holds all in-memory caches (§5) and implements event
//! creation (§3.1) and replication (§3.2) with per-account atomic sections.
//!
//! Seq/epoch identity is per-`(bucket, origin_node_id)` since v1.8 — each
//! bucket this node writes to owns its own independent seq line via
//! `BucketAllocators`, and restarts only bump epochs for buckets that get
//! written to again.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use tokio::sync::{Mutex, mpsc};

use dashmap::DashMap;
use sha2::{Digest, Sha256};
use shardd_types::*;
use tracing::warn;

use crate::bucket_allocator::BucketAllocators;

/// Per-account state held under a Mutex for the atomic section (§3.1).
#[derive(Debug)]
pub struct AccountState {
    pub balance: i64,
    pub event_count: usize,
    /// Reservation holds keyed by the hold event_id.
    pub reservations: BTreeMap<String, ReservationState>,
    /// Hold event_ids whose reservations have been released via hold_release events.
    pub released: HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct ReservationState {
    pub origin_node_id: String,
    pub amount: u64,
    pub expires_at_unix_ms: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct LocalCreateResult {
    /// The "answer" event for this request: the charge for a charge/
    /// settle, the `ReservationCreate` for a reserve, the `HoldRelease`
    /// for a release.
    pub primary_event: Event,
    /// Every event minted during this call. Empty on idempotent retry.
    pub emitted_events: Vec<Event>,
}

/// Input bundle for `create_local_events`. Carries enough fields to
/// distinguish charge / reserve / settle / release modes; see
/// `LocalCreateMode::from_input` for the dispatch logic.
#[derive(Debug, Clone)]
pub(crate) struct LocalCreateInput {
    pub bucket: String,
    pub account: String,
    pub amount: i64,
    pub note: Option<String>,
    pub max_overdraft: u64,
    pub idempotency_nonce: String,
    pub allow_reserved_bucket: bool,
    pub hold_amount: Option<u64>,
    pub hold_expires_at_unix_ms: Option<u64>,
    pub settle_reservation: Option<String>,
    pub release_reservation: Option<String>,
}

impl LocalCreateInput {
    /// Build the input shape used by every legacy charge/credit caller —
    /// no reservation fields, no caller-supplied hold. The
    /// `allow_reserved_bucket` flag is the only knob beyond the basics.
    pub(crate) fn legacy(
        bucket: String,
        account: String,
        amount: i64,
        note: Option<String>,
        max_overdraft: u64,
        idempotency_nonce: String,
        allow_reserved_bucket: bool,
    ) -> Self {
        Self {
            bucket,
            account,
            amount,
            note,
            max_overdraft,
            idempotency_nonce,
            allow_reserved_bucket,
            hold_amount: None,
            hold_expires_at_unix_ms: None,
            settle_reservation: None,
            release_reservation: None,
        }
    }
}

#[derive(Debug, Clone)]
enum LocalCreateMode {
    /// Standard credit or debit. `hold_override` is `Some((amount, expires))`
    /// when the caller explicitly sized the hold; otherwise the node falls
    /// back to its `hold_multiplier × |amount|` default for debits.
    Charge { hold_override: Option<(u64, u64)> },
    /// Pure reservation — `amount == 0`, caller-supplied hold fields.
    Reserve,
    /// One-shot capture against an existing reservation.
    Settle { reservation_id: String },
    /// Cancel an existing reservation; no charge.
    Release { reservation_id: String },
}

impl LocalCreateMode {
    fn from_input(input: &LocalCreateInput) -> Result<Self, CreateLocalEventError> {
        let has_settle = input.settle_reservation.is_some();
        let has_release = input.release_reservation.is_some();
        if has_settle && has_release {
            return Err(CreateLocalEventError::InvalidRequest(
                "settle_reservation and release_reservation are mutually exclusive".into(),
            ));
        }

        // Hold fields must come as a pair when supplied explicitly.
        let explicit_hold = match (input.hold_amount, input.hold_expires_at_unix_ms) {
            (Some(amt), Some(expires)) => Some((amt, expires)),
            (None, None) => None,
            _ => {
                return Err(CreateLocalEventError::InvalidRequest(
                    "hold_amount and hold_expires_at_unix_ms must be set together".into(),
                ));
            }
        };

        if let Some(reservation_id) = input.settle_reservation.clone() {
            if input.amount >= 0 {
                return Err(CreateLocalEventError::InvalidRequest(
                    "settle_reservation requires a debit (amount < 0)".into(),
                ));
            }
            if explicit_hold.is_some() {
                return Err(CreateLocalEventError::InvalidRequest(
                    "settle_reservation cannot be combined with explicit hold fields".into(),
                ));
            }
            return Ok(LocalCreateMode::Settle { reservation_id });
        }

        if let Some(reservation_id) = input.release_reservation.clone() {
            if input.amount != 0 {
                return Err(CreateLocalEventError::InvalidRequest(
                    "release_reservation requires amount == 0".into(),
                ));
            }
            if explicit_hold.is_some() {
                return Err(CreateLocalEventError::InvalidRequest(
                    "release_reservation cannot be combined with explicit hold fields".into(),
                ));
            }
            return Ok(LocalCreateMode::Release { reservation_id });
        }

        if let Some((hold_amount, hold_expires_at_unix_ms)) = explicit_hold {
            if hold_amount == 0 {
                return Err(CreateLocalEventError::InvalidRequest(
                    "hold_amount must be > 0 when supplied".into(),
                ));
            }
            if hold_expires_at_unix_ms <= Event::now_ms() {
                return Err(CreateLocalEventError::InvalidRequest(
                    "hold_expires_at_unix_ms must be in the future".into(),
                ));
            }
            // Pure reserve: amount == 0. Otherwise it's a debit with a
            // caller-sized hold.
            if input.amount == 0 {
                return Ok(LocalCreateMode::Reserve);
            }
            if input.amount > 0 {
                return Err(CreateLocalEventError::InvalidRequest(
                    "credits cannot carry a hold".into(),
                ));
            }
            return Ok(LocalCreateMode::Charge {
                hold_override: Some((hold_amount, hold_expires_at_unix_ms)),
            });
        }

        Ok(LocalCreateMode::Charge {
            hold_override: None,
        })
    }
}

/// Failure modes for `create_local_event(s)`.
#[derive(Debug, Clone)]
pub enum CreateLocalEventError {
    /// The debit would leave the account below its overdraft floor.
    /// Tuple: (balance, available_balance, projected_available).
    InsufficientFunds(i64, i64, i64),
    /// The target bucket name is reserved for internal use (e.g.
    /// `__meta__`, `__billing__<...>`) and never accepts client writes.
    BucketReserved(String),
    /// The target bucket has been hard-deleted via a `BucketDelete`
    /// meta event (§3.5). Deleted names are reserved forever.
    BucketDeleted(String),
    /// Caller passed a combination of fields that doesn't map to any
    /// supported mode (e.g. `settle_reservation` with `amount >= 0`,
    /// or `hold_amount` without `hold_expires_at_unix_ms`).
    InvalidRequest(String),
    /// Settle/release referenced a reservation this node has no record
    /// of. Either the id is wrong or the originating event hasn't
    /// replicated yet — the caller should retry.
    ReservationNotFound(String),
    /// The reservation has already expired; settle/release is no-op
    /// territory because passive expiry has already kicked in.
    ReservationExpired(String),
    /// A prior settle or release has already terminated this reservation.
    ReservationAlreadyReleased(String),
    /// Settle attempted to capture more than was reserved.
    /// Tuple: (reservation_amount, attempted_amount).
    ReservationOverspend(u64, u64),
}

impl AccountState {
    fn new() -> Self {
        Self {
            balance: 0,
            event_count: 0,
            reservations: BTreeMap::new(),
            released: HashSet::new(),
        }
    }

    fn active_reservations<'a>(
        &'a self,
        now_ms: u64,
    ) -> impl Iterator<Item = (&'a String, &'a ReservationState)> + 'a {
        self.reservations
            .iter()
            .filter(move |(event_id, reservation)| {
                reservation.expires_at_unix_ms > now_ms && !self.released.contains(*event_id)
            })
    }

    fn active_hold_total(&self, now_ms: u64) -> u64 {
        self.active_reservations(now_ms)
            .map(|(_, reservation)| reservation.amount)
            .sum()
    }

    fn active_hold_total_for_origin(
        &self,
        origin_node_id: &str,
        now_ms: u64,
        min_remaining_ms: u64,
    ) -> u64 {
        self.active_reservations(now_ms)
            .filter(|(_, reservation)| {
                reservation.origin_node_id == origin_node_id
                    && reservation.expires_at_unix_ms.saturating_sub(now_ms) >= min_remaining_ms
            })
            .map(|(_, reservation)| reservation.amount)
            .sum()
    }

    /// Compute available_balance = balance - active_holds (§11.3).
    pub fn available_balance(&self, now_ms: u64) -> i64 {
        self.balance - self.active_hold_total(now_ms) as i64
    }

    fn track_reservation(&mut self, event: &Event) {
        if event.has_hold() {
            self.reservations.insert(
                event.event_id.clone(),
                ReservationState {
                    origin_node_id: event.origin_node_id.clone(),
                    amount: event.hold_amount,
                    expires_at_unix_ms: event.hold_expires_at_unix_ms,
                },
            );
        }
    }

    fn apply_release(&mut self, event_id: &str) {
        self.released.insert(event_id.to_string());
    }

    fn reservations_by_origin(&self, now_ms: u64) -> BTreeMap<String, OriginReservationSummary> {
        let mut result = BTreeMap::new();
        for (_, reservation) in self.active_reservations(now_ms) {
            let summary = result
                .entry(reservation.origin_node_id.clone())
                .or_insert_with(OriginReservationSummary::default);
            summary.reserved_amount += reservation.amount;
            summary.reservation_count += 1;
            summary.next_expiry_unix_ms = Some(
                summary
                    .next_expiry_unix_ms
                    .map_or(reservation.expires_at_unix_ms, |current| {
                        current.min(reservation.expires_at_unix_ms)
                    }),
            );
            summary.latest_expiry_unix_ms = Some(
                summary
                    .latest_expiry_unix_ms
                    .map_or(reservation.expires_at_unix_ms, |current| {
                        current.max(reservation.expires_at_unix_ms)
                    }),
            );
        }
        result
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HoldConfig {
    pub multiplier: u64,
    pub duration_ms: u64,
}

/// All node state. Generic over storage backend for testability.
#[derive(Clone)]
pub struct SharedState<S: shardd_storage::StorageBackend> {
    pub node_id: Arc<str>,
    pub addr: Arc<str>,
    pub storage: Arc<S>,

    /// Per-`(bucket, self.node_id)` seq/epoch allocator. Replaces the old
    /// node-wide `current_epoch` + `next_seq` pair. Each bucket owns its
    /// own epoch line; buckets we never write to never accumulate epochs.
    pub bucket_allocators: BucketAllocators<S>,

    /// Per-account state under Mutex for atomic section (§3.1).
    accounts: Arc<DashMap<BalanceKey, Arc<Mutex<AccountState>>>>,
    /// Contiguous head per (bucket, origin, epoch) (§2.5).
    heads: Arc<DashMap<EpochKey, u64>>,
    /// Out-of-order sequences per (bucket, origin, epoch) for head advancement (§5.1).
    pending_seqs: Arc<DashMap<EpochKey, BTreeSet<u64>>>,
    /// Per-epoch-key lock for atomic advance_head + drain_pending.
    head_locks: Arc<DashMap<EpochKey, Arc<std::sync::Mutex<()>>>>,
    /// Account → set of (bucket, origin, epoch) triples that contributed events.
    account_origin_epochs: Arc<DashMap<BalanceKey, HashSet<EpochKey>>>,
    /// Max known sequence per (bucket, origin, epoch).
    max_known_seqs: Arc<DashMap<EpochKey, u64>>,
    /// Buffered events still needed for orphan recovery, gap fill, or digest advancement.
    event_buffer: Arc<DashMap<OriginKey, Event>>,
    /// Tracks what's not yet in Postgres: OriginKey → created_at_ms.
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

    /// Rolling prefix digests per (bucket, origin, epoch) (§8.3).
    digests: Arc<DashMap<EpochKey, (u64, [u8; 32])>>,

    /// Buckets that have been hard-deleted via a `BucketDelete` meta
    /// event (§3.5). Value is the `created_at_unix_ms` of the deleting
    /// meta event — useful for audit + tiebreak, but the presence of a
    /// key is what matters for rejection. Rebuilt from the `__meta__`
    /// log on startup; monotonically grows at runtime as new meta
    /// events arrive. Never shrinks — a deleted name is reserved
    /// forever.
    pub deleted_buckets: Arc<DashMap<String, u64>>,
}

impl<S: shardd_storage::StorageBackend> SharedState<S> {
    /// Build a new SharedState. Rebuilds caches from storage on init.
    pub async fn new(
        node_id: String,
        addr: String,
        storage: S,
        batch_tx: mpsc::UnboundedSender<Event>,
        correction_tx: mpsc::UnboundedSender<Event>,
        hold_config: HoldConfig,
    ) -> Self {
        let storage = Arc::new(storage);
        let node_id_arc: Arc<str> = Arc::from(node_id.as_str());
        let accounts: DashMap<BalanceKey, Arc<Mutex<AccountState>>> = DashMap::new();
        let heads: DashMap<EpochKey, u64> = DashMap::new();
        let pending_seqs: DashMap<EpochKey, BTreeSet<u64>> = DashMap::new();
        let account_origin_epochs: DashMap<BalanceKey, HashSet<EpochKey>> = DashMap::new();
        let max_known_seqs: DashMap<EpochKey, u64> = DashMap::new();
        let event_buffer: DashMap<OriginKey, Event> = DashMap::new();
        let idempotency_cache: DashMap<(String, String, String, i64), Event> = DashMap::new();
        let digests: DashMap<EpochKey, (u64, [u8; 32])> = DashMap::new();
        let mut total_events = 0usize;

        // §13.1: prepare per-bucket allocators. `load_from_storage` flags
        // every existing `(bucket, node_id)` row `needs_bump = TRUE` in the
        // DB; the first write to each bucket after startup atomically
        // bumps that bucket's epoch.
        let bucket_allocators = BucketAllocators::new(node_id_arc.clone(), storage.clone());
        if let Err(error) = bucket_allocators.load_from_storage().await {
            warn!(error = %error, "failed to load bucket allocators; starting fresh");
        }

        // Rebuild balances from storage.
        if let Ok(balances) = storage.aggregate_balances().await {
            for (bucket, account, sum) in balances {
                let key = (bucket, account);
                accounts.insert(
                    key,
                    Arc::new(Mutex::new(AccountState {
                        balance: sum,
                        event_count: 0,
                        reservations: BTreeMap::new(),
                        released: HashSet::new(),
                    })),
                );
            }
        }

        // Rebuild heads + max_known from storage.
        if let Ok(seqs_by_epoch) = storage.sequences_by_origin_epoch().await {
            for (epoch_key, seqs) in &seqs_by_epoch {
                total_events += seqs.len();
                let head = compute_contiguous_head(seqs);
                heads.insert(epoch_key.clone(), head);
                let pending: BTreeSet<u64> =
                    seqs.iter().copied().filter(|seq| *seq > head).collect();
                if !pending.is_empty() {
                    pending_seqs.insert(epoch_key.clone(), pending);
                }
                if let Some(&max) = seqs.last() {
                    max_known_seqs.insert(epoch_key.clone(), max);
                }
            }
        }

        // Rebuild balance_key → {epoch_key} reverse index.
        if let Ok(mapping) = storage.origin_account_epoch_mapping().await {
            for (bucket, origin, epoch, account) in mapping {
                account_origin_epochs
                    .entry((bucket.clone(), account))
                    .or_default()
                    .insert((bucket, origin, epoch));
            }
        }

        // Rebuild per-event caches from storage.
        let now_ms = Event::now_ms();
        if let Ok(all_events) = storage.query_all_events_sorted().await {
            for event in &all_events {
                let acct = accounts
                    .entry(event.balance_key())
                    .or_insert_with(|| Arc::new(Mutex::new(AccountState::new())))
                    .clone();
                let mut state = acct.lock().await;
                state.event_count += 1;
                if event.has_hold() && event.hold_expires_at_unix_ms > now_ms {
                    state.track_reservation(event);
                }
                if event.r#type == EventType::HoldRelease
                    && let Some(ref void_ref) = event.void_ref
                {
                    state.apply_release(void_ref);
                }
            }

            // §10.3: Rebuild idempotency cache from recent events in DB.
            for event in all_events.iter().rev().take(10000) {
                let key = (
                    event.idempotency_nonce.clone(),
                    event.bucket.clone(),
                    event.account.clone(),
                    event.amount,
                );
                idempotency_cache
                    .entry(key)
                    .or_insert_with(|| event.clone());
            }

            // Rebuild the pending-event buffer and recompute rolling digests
            // from durable truth so restart preserves replay safety.
            let mut digest_events = all_events;
            digest_events.sort_by(|a, b| {
                a.bucket
                    .cmp(&b.bucket)
                    .then_with(|| a.origin_node_id.cmp(&b.origin_node_id))
                    .then_with(|| a.origin_epoch.cmp(&b.origin_epoch))
                    .then_with(|| a.origin_seq.cmp(&b.origin_seq))
            });

            let mut computed_digests: BTreeMap<EpochKey, (u64, [u8; 32])> = BTreeMap::new();
            for event in &digest_events {
                let epoch_key = event.epoch_key();
                let head = heads.get(&epoch_key).map(|v| *v).unwrap_or(0);

                if event.origin_seq > head {
                    event_buffer.insert(event.origin_key(), event.clone());
                    continue;
                }

                let (digest_head, current_digest) =
                    computed_digests.entry(epoch_key).or_insert((0, [0u8; 32]));

                if event.origin_seq != *digest_head + 1 {
                    continue;
                }

                let event_hash = Sha256::digest(event.canonical().as_bytes());
                let mut hasher = Sha256::new();
                hasher.update(*current_digest);
                hasher.update(event_hash);
                *current_digest = hasher.finalize().into();
                *digest_head = event.origin_seq;
            }

            for ((bucket, origin, epoch), (head, digest)) in computed_digests {
                digests.insert((bucket.clone(), origin.clone(), epoch), (head, digest));
                let _ = storage
                    .save_digest(&bucket, &origin, epoch, head, &digest)
                    .await;
            }
        }

        // §3.5: replay the __meta__ log to rebuild `deleted_buckets`.
        // The meta log replicates like any other bucket and is NEVER
        // deleted, so even a restarted node rebuilds the full tombstone
        // set from durable truth. Any meta events we haven't seen yet
        // will arrive via normal catch-up.
        let deleted_buckets: DashMap<String, u64> = DashMap::new();
        if let Ok(meta_events) = storage.query_events_by_bucket(META_BUCKET).await {
            for event in meta_events {
                if let Some(target) = event.meta_target_bucket() {
                    deleted_buckets.insert(target.to_string(), event.created_at_unix_ms);
                }
            }
        }

        Self {
            node_id: node_id_arc,
            addr: Arc::from(addr.as_str()),
            storage,
            bucket_allocators,
            accounts: Arc::new(accounts),
            heads: Arc::new(heads),
            pending_seqs: Arc::new(pending_seqs),
            head_locks: Arc::new(DashMap::new()),
            account_origin_epochs: Arc::new(account_origin_epochs),
            max_known_seqs: Arc::new(max_known_seqs),
            event_buffer: Arc::new(event_buffer),
            unpersisted: Arc::new(DashMap::new()),
            idempotency_cache: Arc::new(idempotency_cache),
            batch_tx,
            correction_tx,
            total_event_count: Arc::new(AtomicUsize::new(total_events)),
            hold_multiplier: hold_config.multiplier,
            hold_duration_ms: hold_config.duration_ms,
            phase: Arc::new(std::sync::atomic::AtomicU8::new(0)), // 0 = Warming
            digests: Arc::new(digests),
            deleted_buckets: Arc::new(deleted_buckets),
        }
    }

    // ── Event creation (§3.1) ────────────────────────────────────────

    /// Create a local charge/credit event within the per-account atomic section.
    pub async fn create_local_event(
        &self,
        bucket: String,
        account: String,
        amount: i64,
        note: Option<String>,
        max_overdraft: u64,
        idempotency_nonce: String,
    ) -> Result<Event, CreateLocalEventError> {
        self.create_local_events(LocalCreateInput::legacy(
            bucket,
            account,
            amount,
            note,
            max_overdraft,
            idempotency_nonce,
            false,
        ))
        .await
        .map(|result| result.primary_event)
    }

    pub(crate) async fn create_local_events(
        &self,
        input: LocalCreateInput,
    ) -> Result<LocalCreateResult, CreateLocalEventError> {
        // §3.5: reject writes to reserved and tombstoned buckets before
        // any side effects. The `allow_reserved_bucket` opt-in is set
        // by the gateway's internal billing route; client RPC paths
        // never pass true.
        if !input.allow_reserved_bucket && is_reserved_bucket_name(&input.bucket) {
            return Err(CreateLocalEventError::BucketReserved(input.bucket));
        }
        if self.deleted_buckets.contains_key(&input.bucket) {
            return Err(CreateLocalEventError::BucketDeleted(input.bucket));
        }

        // Validate request shape and pick a mode (§11.2). Settle and
        // release are mutually exclusive with explicit `hold_amount`,
        // and each has a fixed sign requirement on `amount`.
        let mode = LocalCreateMode::from_input(&input)?;

        let key = (input.bucket.clone(), input.account.clone());
        let acct = self
            .accounts
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(AccountState::new())))
            .clone();

        // Hold the per-account lock for the entire atomic section (§3.1)
        let mut state = acct.lock().await;

        // Step 1: Idempotency check (§10.3) — same primary nonce for
        // every mode; a retry with the same nonce returns the cached
        // primary event regardless of which branch produced it.
        let idem_key = (
            input.idempotency_nonce.clone(),
            input.bucket.clone(),
            input.account.clone(),
            input.amount,
        );

        if let Some(existing) = self.idempotency_cache.get(&idem_key) {
            return Ok(LocalCreateResult {
                primary_event: existing.value().clone(),
                emitted_events: Vec::new(),
            });
        }

        // DB fallback (§10.3 step 2): cache may have been evicted after restart
        if let Ok(matches) = self
            .storage
            .find_by_idempotency_key(
                &input.idempotency_nonce,
                &input.bucket,
                &input.account,
                input.amount,
            )
            .await
            && !matches.is_empty()
        {
            // Determine canonical winner per §10.4
            let winner = matches
                .iter()
                .min_by(|a, b| {
                    a.created_at_unix_ms
                        .cmp(&b.created_at_unix_ms)
                        .then_with(|| a.event_id.cmp(&b.event_id))
                })
                .unwrap()
                .clone();

            self.idempotency_cache.insert(idem_key, winner.clone());
            return Ok(LocalCreateResult {
                primary_event: winner,
                emitted_events: Vec::new(),
            });
        }

        let now_ms = Event::now_ms();

        match mode {
            LocalCreateMode::Charge { hold_override } => {
                self.emit_charge_locked(&mut state, input, hold_override, now_ms)
                    .await
            }
            LocalCreateMode::Reserve => self.emit_reserve_locked(&mut state, input, now_ms).await,
            LocalCreateMode::Settle { reservation_id } => {
                self.emit_settle_locked(&mut state, input, &reservation_id, now_ms)
                    .await
            }
            LocalCreateMode::Release { reservation_id } => {
                self.emit_release_locked(&mut state, input, &reservation_id, now_ms)
                    .await
            }
        }
    }

    /// Charge/credit branch — the legacy flow plus an optional caller-
    /// supplied hold override. When `hold_override` is `None`, the
    /// implicit `hold_multiplier × |amount|` sizing kicks in on debits.
    async fn emit_charge_locked(
        &self,
        state: &mut AccountState,
        input: LocalCreateInput,
        hold_override: Option<(u64, u64)>,
        now_ms: u64,
    ) -> Result<LocalCreateResult, CreateLocalEventError> {
        let (hold_amount, hold_expires_at_unix_ms) = match hold_override {
            Some((amt, expires)) => {
                // Explicit caller hold — honour as-is. Don't subtract
                // existing per-origin holds; the caller knows what they
                // want reserved for this debit specifically.
                (amt, expires)
            }
            None => {
                let requested_hold = if input.amount < 0 && self.hold_multiplier > 0 {
                    input.amount.unsigned_abs() * self.hold_multiplier
                } else {
                    0
                };
                let renewal_window_ms = if self.hold_duration_ms == 0 {
                    0
                } else {
                    self.hold_duration_ms.div_ceil(10)
                };
                let current_hold = state.active_hold_total_for_origin(
                    self.node_id.as_ref(),
                    now_ms,
                    renewal_window_ms,
                );
                let hold_amount = requested_hold.saturating_sub(current_hold);
                let hold_expires_at_unix_ms = if hold_amount > 0 {
                    now_ms + self.hold_duration_ms
                } else {
                    0
                };
                (hold_amount, hold_expires_at_unix_ms)
            }
        };

        // Overdraft guard (§9.1) — debits only.
        if input.amount < 0 {
            let avail = state.available_balance(now_ms);
            let projected = avail + input.amount - (hold_amount as i64);
            let floor = -(input.max_overdraft as i64);
            if projected < floor {
                return Err(CreateLocalEventError::InsufficientFunds(
                    state.balance,
                    avail,
                    projected,
                ));
            }
        }

        let mut emitted_events = Vec::new();

        // Optional reservation event before the charge.
        if hold_amount > 0 {
            let (reservation_epoch, reservation_seq) = self
                .allocate_or_fail(
                    &input.bucket,
                    state.balance,
                    state.available_balance(now_ms),
                )
                .await?;
            let reservation_event = Event {
                event_id: Event::generate_id(),
                origin_node_id: self.node_id.to_string(),
                origin_epoch: reservation_epoch,
                origin_seq: reservation_seq,
                created_at_unix_ms: now_ms,
                r#type: EventType::ReservationCreate,
                bucket: input.bucket.clone(),
                account: input.account.clone(),
                amount: 0,
                note: None,
                idempotency_nonce: format!("reserve:{}", input.idempotency_nonce),
                void_ref: None,
                hold_amount,
                hold_expires_at_unix_ms,
            };
            state.event_count += 1;
            state.track_reservation(&reservation_event);
            self.idempotency_cache.insert(
                (
                    reservation_event.idempotency_nonce.clone(),
                    input.bucket.clone(),
                    input.account.clone(),
                    0,
                ),
                reservation_event.clone(),
            );
            emitted_events.push(reservation_event);
        }

        // The charge/credit Standard event itself.
        let (charge_epoch, charge_seq) = self
            .allocate_or_fail(
                &input.bucket,
                state.balance,
                state.available_balance(now_ms),
            )
            .await?;
        let charge_event = Event {
            event_id: Event::generate_id(),
            origin_node_id: self.node_id.to_string(),
            origin_epoch: charge_epoch,
            origin_seq: charge_seq,
            created_at_unix_ms: now_ms,
            r#type: EventType::Standard,
            bucket: input.bucket.clone(),
            account: input.account.clone(),
            amount: input.amount,
            note: input.note,
            idempotency_nonce: input.idempotency_nonce.clone(),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };
        state.balance += input.amount;
        state.event_count += 1;
        self.idempotency_cache.insert(
            (
                input.idempotency_nonce.clone(),
                input.bucket.clone(),
                input.account.clone(),
                input.amount,
            ),
            charge_event.clone(),
        );

        emitted_events.push(charge_event.clone());
        self.flush_local_events(&emitted_events).await;

        Ok(LocalCreateResult {
            primary_event: charge_event,
            emitted_events,
        })
    }

    /// Pure reserve — caller supplied `hold_amount` + `hold_expires_at_unix_ms`,
    /// `amount == 0`. Mints exactly one `ReservationCreate` event.
    async fn emit_reserve_locked(
        &self,
        state: &mut AccountState,
        input: LocalCreateInput,
        now_ms: u64,
    ) -> Result<LocalCreateResult, CreateLocalEventError> {
        let hold_amount = input.hold_amount.unwrap_or(0);
        let hold_expires_at_unix_ms = input.hold_expires_at_unix_ms.unwrap_or(0);

        // Pre-validated by `LocalCreateMode::from_input`, but cheap to assert.
        debug_assert!(hold_amount > 0);
        debug_assert!(hold_expires_at_unix_ms > now_ms);

        // The reservation reduces available_balance, so the same overdraft
        // guard applies: a reserve we couldn't pay out is a reserve we
        // shouldn't accept.
        let avail = state.available_balance(now_ms);
        let projected = avail - (hold_amount as i64);
        let floor = -(input.max_overdraft as i64);
        if projected < floor {
            return Err(CreateLocalEventError::InsufficientFunds(
                state.balance,
                avail,
                projected,
            ));
        }

        let (epoch, seq) = self
            .allocate_or_fail(&input.bucket, state.balance, avail)
            .await?;
        let event = Event {
            event_id: Event::generate_id(),
            origin_node_id: self.node_id.to_string(),
            origin_epoch: epoch,
            origin_seq: seq,
            created_at_unix_ms: now_ms,
            r#type: EventType::ReservationCreate,
            bucket: input.bucket.clone(),
            account: input.account.clone(),
            amount: 0,
            note: input.note,
            idempotency_nonce: input.idempotency_nonce.clone(),
            void_ref: None,
            hold_amount,
            hold_expires_at_unix_ms,
        };
        state.event_count += 1;
        state.track_reservation(&event);
        self.idempotency_cache.insert(
            (input.idempotency_nonce, input.bucket, input.account, 0),
            event.clone(),
        );

        let emitted = vec![event.clone()];
        self.flush_local_events(&emitted).await;

        Ok(LocalCreateResult {
            primary_event: event,
            emitted_events: emitted,
        })
    }

    /// One-shot capture: emit the Standard charge AND a `HoldRelease`
    /// referencing the reservation, atomically. Releases the full hold
    /// regardless of how much was captured — any unused remainder is
    /// implicitly returned to `available_balance`.
    async fn emit_settle_locked(
        &self,
        state: &mut AccountState,
        input: LocalCreateInput,
        reservation_id: &str,
        now_ms: u64,
    ) -> Result<LocalCreateResult, CreateLocalEventError> {
        let reservation = state
            .reservations
            .get(reservation_id)
            .cloned()
            .ok_or_else(|| {
                CreateLocalEventError::ReservationNotFound(reservation_id.to_string())
            })?;
        if state.released.contains(reservation_id) {
            return Err(CreateLocalEventError::ReservationAlreadyReleased(
                reservation_id.to_string(),
            ));
        }
        if reservation.expires_at_unix_ms <= now_ms {
            return Err(CreateLocalEventError::ReservationExpired(
                reservation_id.to_string(),
            ));
        }
        let attempted = input.amount.unsigned_abs();
        if attempted > reservation.amount {
            return Err(CreateLocalEventError::ReservationOverspend(
                reservation.amount,
                attempted,
            ));
        }

        // Allocate both events up-front so neither lands without the other.
        let (charge_epoch, charge_seq) = self
            .allocate_or_fail(
                &input.bucket,
                state.balance,
                state.available_balance(now_ms),
            )
            .await?;
        let (release_epoch, release_seq) = self
            .allocate_or_fail(
                &input.bucket,
                state.balance,
                state.available_balance(now_ms),
            )
            .await?;

        let charge_event = Event {
            event_id: Event::generate_id(),
            origin_node_id: self.node_id.to_string(),
            origin_epoch: charge_epoch,
            origin_seq: charge_seq,
            created_at_unix_ms: now_ms,
            r#type: EventType::Standard,
            bucket: input.bucket.clone(),
            account: input.account.clone(),
            amount: input.amount,
            note: input.note,
            idempotency_nonce: input.idempotency_nonce.clone(),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };
        let release_event = Event {
            event_id: Event::generate_id(),
            origin_node_id: self.node_id.to_string(),
            origin_epoch: release_epoch,
            origin_seq: release_seq,
            created_at_unix_ms: now_ms,
            r#type: EventType::HoldRelease,
            bucket: input.bucket.clone(),
            account: input.account.clone(),
            amount: 0,
            note: None,
            idempotency_nonce: format!("release:{}", input.idempotency_nonce),
            void_ref: Some(reservation_id.to_string()),
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };

        state.balance += input.amount;
        state.event_count += 2;
        state.apply_release(reservation_id);

        self.idempotency_cache.insert(
            (
                input.idempotency_nonce.clone(),
                input.bucket.clone(),
                input.account.clone(),
                input.amount,
            ),
            charge_event.clone(),
        );
        self.idempotency_cache.insert(
            (
                release_event.idempotency_nonce.clone(),
                input.bucket.clone(),
                input.account.clone(),
                0,
            ),
            release_event.clone(),
        );

        let emitted = vec![charge_event.clone(), release_event];
        self.flush_local_events(&emitted).await;

        Ok(LocalCreateResult {
            primary_event: charge_event,
            emitted_events: emitted,
        })
    }

    /// Cancel a reservation outright. Emits only a `HoldRelease` —
    /// no balance change, no charge event.
    async fn emit_release_locked(
        &self,
        state: &mut AccountState,
        input: LocalCreateInput,
        reservation_id: &str,
        now_ms: u64,
    ) -> Result<LocalCreateResult, CreateLocalEventError> {
        let reservation = state
            .reservations
            .get(reservation_id)
            .cloned()
            .ok_or_else(|| {
                CreateLocalEventError::ReservationNotFound(reservation_id.to_string())
            })?;
        if state.released.contains(reservation_id) {
            return Err(CreateLocalEventError::ReservationAlreadyReleased(
                reservation_id.to_string(),
            ));
        }
        if reservation.expires_at_unix_ms <= now_ms {
            return Err(CreateLocalEventError::ReservationExpired(
                reservation_id.to_string(),
            ));
        }

        let (epoch, seq) = self
            .allocate_or_fail(
                &input.bucket,
                state.balance,
                state.available_balance(now_ms),
            )
            .await?;
        let release_event = Event {
            event_id: Event::generate_id(),
            origin_node_id: self.node_id.to_string(),
            origin_epoch: epoch,
            origin_seq: seq,
            created_at_unix_ms: now_ms,
            r#type: EventType::HoldRelease,
            bucket: input.bucket.clone(),
            account: input.account.clone(),
            amount: 0,
            note: input.note,
            idempotency_nonce: input.idempotency_nonce.clone(),
            void_ref: Some(reservation_id.to_string()),
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };
        state.event_count += 1;
        state.apply_release(reservation_id);
        self.idempotency_cache.insert(
            (input.idempotency_nonce, input.bucket, input.account, 0),
            release_event.clone(),
        );

        let emitted = vec![release_event.clone()];
        self.flush_local_events(&emitted).await;

        Ok(LocalCreateResult {
            primary_event: release_event,
            emitted_events: emitted,
        })
    }

    async fn allocate_or_fail(
        &self,
        bucket: &str,
        balance: i64,
        available: i64,
    ) -> Result<(u32, u64), CreateLocalEventError> {
        match self.bucket_allocators.allocate(bucket).await {
            Ok(pair) => Ok(pair),
            Err(error) => {
                warn!(error = %error, bucket = %bucket, "bucket allocator failed");
                Err(CreateLocalEventError::InsufficientFunds(
                    balance, available, balance,
                ))
            }
        }
    }

    async fn flush_local_events(&self, events: &[Event]) {
        for event in events {
            self.record_new_local_event(event).await;
        }
    }

    // ── Event replication (§3.2) ─────────────────────────────────────

    /// Insert a replicated event. Returns true if newly inserted.
    pub async fn insert_event(&self, event: &Event) -> bool {
        let key = event.origin_key();

        // §3.5: reject replicated events whose bucket is already tombstoned
        // (e.g. a laggy peer is still gossiping pre-delete writes, or a
        // zombie peer came back online with stale data). The meta log
        // itself is NOT affected here — `__meta__` is never in
        // `deleted_buckets`, so `BucketDelete` events pass through.
        if self.deleted_buckets.contains_key(&event.bucket) {
            return false;
        }

        if self.event_is_present(event) {
            return false;
        }

        // Insert into event_buffer atomically (entry API holds shard lock)
        use dashmap::mapref::entry::Entry;
        match self.event_buffer.entry(key.clone()) {
            Entry::Occupied(_) => return false, // another thread got there first
            Entry::Vacant(v) => {
                v.insert(event.clone());
            }
        }

        // §3.5: if this is a meta `BucketDelete` event, cascade NOW,
        // before we touch balances — meta events never affect account
        // balances (amount=0). The cascade wipes every trace of the
        // target bucket across storage + in-memory DashMaps.
        if let Some(target) = event.meta_target_bucket() {
            let target = target.to_string();
            if target != META_BUCKET {
                self.deleted_buckets
                    .insert(target.clone(), event.created_at_unix_ms);
                self.apply_bucket_delete_cascade(&target).await;
            }
        }

        // Update account state
        let acct_key = event.balance_key();
        let acct = self
            .accounts
            .entry(acct_key)
            .or_insert_with(|| Arc::new(Mutex::new(AccountState::new())))
            .clone();

        // Use try_lock to avoid deadlock (replicated events don't need the full atomic section)
        // If lock is held by a local create, the balance update is safe because
        // replicated events don't check overdraft.
        let mut state = acct.lock().await;
        state.balance += event.amount;
        state.event_count += 1;

        // Track holds from replicated events
        state.track_reservation(event);
        if event.r#type == EventType::HoldRelease
            && let Some(ref void_ref) = event.void_ref
        {
            state.apply_release(void_ref);
        }
        drop(state);

        // Update non-account caches
        self.unpersisted.insert(key, event.created_at_unix_ms);
        self.advance_head(&event.epoch_key(), event.origin_seq)
            .await;
        self.update_origin_tracking(event);
        self.total_event_count.fetch_add(1, Relaxed);

        // Queue for async persistence to this node's own PG
        let _ = self.batch_tx.send(event.clone());

        // §10.4: Cross-node idempotency conflict check
        self.check_idempotency_conflict(event).await;

        true
    }

    // ── Meta log (§3.5) ────────────────────────────────────────────────

    /// Create and apply a `BucketDelete` meta event for `target_bucket`
    /// locally, then publish it via the normal broadcast path. Caller
    /// must already have verified authorization and that the bucket is
    /// not already tombstoned or reserved; this method is the final
    /// write path. Returns the meta event so the caller can await its
    /// acks if they care.
    pub async fn create_meta_bucket_delete(
        &self,
        target_bucket: &str,
        reason: Option<String>,
    ) -> Result<Event, CreateLocalEventError> {
        if target_bucket == META_BUCKET || is_reserved_bucket_name(target_bucket) {
            return Err(CreateLocalEventError::BucketReserved(target_bucket.into()));
        }
        if self.deleted_buckets.contains_key(target_bucket) {
            return Err(CreateLocalEventError::BucketDeleted(target_bucket.into()));
        }

        // Allocate a seq in the `__meta__` bucket's own line (lazy-bump
        // applies normally). Meta writes don't need the per-account
        // atomic section; no balance is touched.
        let (epoch, seq) = self
            .bucket_allocators
            .allocate(META_BUCKET)
            .await
            .map_err(|error| {
                warn!(error = %error, "meta bucket allocator failed");
                CreateLocalEventError::InsufficientFunds(0, 0, 0)
            })?;

        let now_ms = Event::now_ms();
        let event = Event {
            event_id: Event::generate_id(),
            origin_node_id: self.node_id.to_string(),
            origin_epoch: epoch,
            origin_seq: seq,
            created_at_unix_ms: now_ms,
            r#type: EventType::BucketDelete,
            bucket: META_BUCKET.to_string(),
            account: target_bucket.to_string(),
            amount: 0,
            note: reason,
            // Natural key: one delete tombstone per target bucket name.
            // A retry with the same name dedupes to the same event.
            idempotency_nonce: format!("delete:{target_bucket}"),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };

        // Apply locally first (record tombstone + cascade), then
        // persist + broadcast via the standard new-event path.
        self.deleted_buckets
            .insert(target_bucket.to_string(), now_ms);
        self.apply_bucket_delete_cascade(target_bucket).await;
        self.record_new_local_event(&event).await;

        Ok(event)
    }

    /// Drop every trace of `bucket` from this node — storage rows,
    /// in-memory DashMaps, allocator state. Idempotent; a second call
    /// is a safe no-op. Called from both the local `create_meta_bucket_delete`
    /// path and from `insert_event` when a replicated `BucketDelete`
    /// arrives.
    ///
    /// Refuses to operate on `META_BUCKET` (the meta log itself is
    /// never deleted; `create_meta_bucket_delete` rejects that input
    /// and `insert_event`'s `target != META_BUCKET` guard mirrors it).
    async fn apply_bucket_delete_cascade(&self, bucket: &str) {
        if bucket == META_BUCKET {
            return;
        }

        // Storage side — 3 DELETEs in a single transaction.
        if let Err(error) = self.storage.delete_bucket_cascade(bucket).await {
            warn!(error = %error, bucket = %bucket, "bucket cascade: storage delete failed");
        }

        // In-memory side — strip every DashMap entry keyed by this
        // bucket. Order doesn't matter; each removal is independent.
        self.heads.retain(|(b, _, _), _| b != bucket);
        self.pending_seqs.retain(|(b, _, _), _| b != bucket);
        self.head_locks.retain(|(b, _, _), _| b != bucket);
        self.max_known_seqs.retain(|(b, _, _), _| b != bucket);
        self.digests.retain(|(b, _, _), _| b != bucket);
        self.event_buffer.retain(|(b, _, _, _), _| b != bucket);
        self.unpersisted.retain(|(b, _, _, _), _| b != bucket);
        self.accounts.retain(|(b, _), _| b != bucket);
        self.account_origin_epochs.retain(|(b, _), _| b != bucket);
        self.idempotency_cache.retain(|(_, b, _, _), _| b != bucket);
        self.bucket_allocators.forget(bucket);
    }

    /// Check for idempotency conflicts and emit corrections (§10.4-10.6).
    async fn check_idempotency_conflict(&self, event: &Event) {
        let idem_key = (
            event.idempotency_nonce.clone(),
            event.bucket.clone(),
            event.account.clone(),
            event.amount,
        );

        // Check if we already have an event with this idempotency key
        let existing = self
            .idempotency_cache
            .get(&idem_key)
            .map(|e| e.value().clone());
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
        let void_idem = (
            void_nonce.clone(),
            loser.bucket.clone(),
            loser.account.clone(),
            -loser.amount,
        );

        if self.idempotency_cache.contains_key(&void_idem) {
            return; // Already emitted (or another node did)
        }

        // Check DB too
        if let Ok(matches) = self
            .storage
            .find_by_idempotency_key(&void_nonce, &loser.bucket, &loser.account, -loser.amount)
            .await
            && !matches.is_empty()
        {
            return;
        }

        // Emit void event — allocated from the loser's bucket's seq space
        // so the void and its target live in the same per-bucket chain.
        let (void_epoch, void_seq) = match self.bucket_allocators.allocate(&loser.bucket).await {
            Ok(pair) => pair,
            Err(error) => {
                warn!(error = %error, bucket = %loser.bucket, "allocator failed for void event");
                return;
            }
        };
        let void_event = Event {
            event_id: Event::generate_id(),
            origin_node_id: self.node_id.to_string(),
            origin_epoch: void_epoch,
            origin_seq: void_seq,
            created_at_unix_ms: Event::now_ms(),
            r#type: EventType::Void,
            bucket: loser.bucket.clone(),
            account: loser.account.clone(),
            amount: -loser.amount,
            note: Some(format!("void: duplicate of event {}", winner.event_id)),
            idempotency_nonce: void_nonce.clone(),
            void_ref: Some(loser.event_id.clone()),
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };

        // Apply void to state
        self.apply_correction_event(&void_event).await;

        // §10.5 step 2: If loser had a hold, emit hold_release
        if loser.has_hold() {
            let release_nonce = format!("release:{}", loser.event_id);
            let release_idem = (
                release_nonce.clone(),
                loser.bucket.clone(),
                loser.account.clone(),
                0,
            );

            if !self.idempotency_cache.contains_key(&release_idem) {
                let (release_epoch, release_seq) = match self
                    .bucket_allocators
                    .allocate(&loser.bucket)
                    .await
                {
                    Ok(pair) => pair,
                    Err(error) => {
                        warn!(error = %error, bucket = %loser.bucket, "allocator failed for hold-release");
                        return;
                    }
                };
                let release_event = Event {
                    event_id: Event::generate_id(),
                    origin_node_id: self.node_id.to_string(),
                    origin_epoch: release_epoch,
                    origin_seq: release_seq,
                    created_at_unix_ms: Event::now_ms(),
                    r#type: EventType::HoldRelease,
                    bucket: loser.bucket.clone(),
                    account: loser.account.clone(),
                    amount: 0,
                    note: Some(format!(
                        "release hold: duplicate of event {}",
                        winner.event_id
                    )),
                    idempotency_nonce: release_nonce,
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
        let acct = self
            .accounts
            .entry(acct_key)
            .or_insert_with(|| Arc::new(Mutex::new(AccountState::new())))
            .clone();
        let mut state = acct.lock().await;
        state.balance += event.amount;
        state.event_count += 1;
        if event.r#type == EventType::HoldRelease
            && let Some(ref void_ref) = event.void_ref
        {
            state.apply_release(void_ref);
        }
        drop(state);

        self.advance_head(&event.epoch_key(), event.origin_seq)
            .await;
        self.update_origin_tracking(event);
        self.total_event_count.fetch_add(1, Relaxed);

        // Install correction in idempotency cache
        self.idempotency_cache.insert(
            (
                event.idempotency_nonce.clone(),
                event.bucket.clone(),
                event.account.clone(),
                event.amount,
            ),
            event.clone(),
        );

        let _ = self.batch_tx.send(event.clone());
        // §4.1: Broadcast correction events to peers
        let _ = self.correction_tx.send(event.clone());
    }

    /// Insert a batch of events. Returns count of newly inserted.
    pub async fn insert_events_batch(&self, events: &[Event]) -> usize {
        let mut count = 0;
        for event in events {
            if self.insert_event(event).await {
                count += 1;
            }
        }
        count
    }

    // ── Reads (in-memory) ────────────────────────────────────────────

    pub fn total_balance(&self) -> i64 {
        self.accounts
            .iter()
            .map(|e| {
                // Use try_lock to avoid blocking
                e.value().try_lock().map(|s| s.balance).unwrap_or(0)
            })
            .sum()
    }

    pub fn account_balance(&self, bucket: &str, account: &str) -> i64 {
        self.accounts
            .get(&(bucket.to_string(), account.to_string()))
            .and_then(|a| a.try_lock().ok().map(|s| s.balance))
            .unwrap_or(0)
    }

    pub fn account_available_balance(&self, bucket: &str, account: &str) -> i64 {
        let now_ms = Event::now_ms();
        self.accounts
            .get(&(bucket.to_string(), account.to_string()))
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
                    active_hold_total: s.active_hold_total(now_ms) as i64,
                    reserved_by_origin: s.reservations_by_origin(now_ms),
                    event_count: s.event_count,
                });
            }
        }
        result.sort_by(|a, b| {
            a.bucket
                .cmp(&b.bucket)
                .then_with(|| a.account.cmp(&b.account))
        });
        result
    }

    pub fn get_heads(&self) -> BTreeMap<String, u64> {
        // Key format: "{bucket}\t{origin}:{epoch}". Tab separator because
        // bucket names can contain colons and underscores, and we want the
        // BTreeMap to sort bucket-first so catch-up can batch-fetch within
        // a bucket cleanly.
        self.heads
            .iter()
            .map(|e| {
                let (bucket, origin, epoch) = e.key();
                (format!("{bucket}\t{origin}:{epoch}"), *e.value())
            })
            .collect()
    }

    /// Highest-seen `origin_seq` per `(bucket, origin, epoch)`, keyed
    /// identically to `get_heads()` so the admin UI can zip them into
    /// a replication-status annotation per event.
    pub fn get_max_known_seqs(&self) -> BTreeMap<String, u64> {
        self.max_known_seqs
            .iter()
            .map(|e| {
                let (bucket, origin, epoch) = e.key();
                (format!("{bucket}\t{origin}:{epoch}"), *e.value())
            })
            .collect()
    }

    pub fn event_count(&self) -> usize {
        self.total_event_count.load(Relaxed)
    }

    pub fn sync_gap(&self) -> u64 {
        self.max_known_seqs
            .iter()
            .map(|entry| {
                let epoch_key = entry.key();
                let max_known = *entry.value();
                let head = self.heads.get(epoch_key).map(|value| *value).unwrap_or(0);
                max_known.saturating_sub(head)
            })
            .max()
            .unwrap_or(0)
    }

    /// Per-bucket view of the sync gap. For each bucket, returns the max
    /// `(max_known − head)` across every `(origin_node_id, origin_epoch)`
    /// entry that shares that bucket. Drives the
    /// `shardd_node_sync_gap_per_bucket` Prometheus series so operators
    /// can see which bucket is responsible for a cluster-wide gap spike.
    pub fn sync_gap_per_bucket(&self) -> BTreeMap<String, u64> {
        let mut per_bucket: BTreeMap<String, u64> = BTreeMap::new();
        for entry in self.max_known_seqs.iter() {
            let (bucket, _, _) = entry.key();
            let max_known = *entry.value();
            let head = self.heads.get(entry.key()).map(|v| *v).unwrap_or(0);
            let gap = max_known.saturating_sub(head);
            let slot = per_bucket.entry(bucket.clone()).or_default();
            if gap > *slot {
                *slot = gap;
            }
        }
        per_bucket
    }

    /// §8.3: Get rolling prefix digests per (bucket, origin, epoch). Key
    /// format matches `get_heads` ("{bucket}\t{origin}:{epoch}") so the
    /// two maps align for cross-node convergence checks.
    pub fn get_digests(&self) -> BTreeMap<String, (u64, String)> {
        self.digests
            .iter()
            .map(|e| {
                let (bucket, origin, epoch) = e.key();
                let (head, digest) = e.value();
                let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
                (format!("{bucket}\t{origin}:{epoch}"), (*head, hex))
            })
            .collect()
    }

    /// Sweep expired holds from all accounts (§5.3).
    /// Call periodically from a background task.
    pub async fn sweep_expired_holds(&self) {
        let now_ms = Event::now_ms();
        for entry in self.accounts.iter() {
            if let Ok(mut state) = entry.value().try_lock() {
                // Collect expired hold event_ids
                let expired: Vec<String> = state
                    .reservations
                    .iter()
                    .filter(|(_, reservation)| reservation.expires_at_unix_ms <= now_ms)
                    .map(|(event_id, _)| event_id.clone())
                    .collect();

                // Remove expired holds
                for event_id in &expired {
                    state.reservations.remove(event_id);
                }

                // Evict release markers for expired holds
                for event_id in &expired {
                    state.released.remove(event_id);
                }
            }
        }
    }

    pub fn get_events_from_buffer(
        &self,
        bucket: &str,
        origin: &str,
        epoch: u32,
        from_seq: u64,
        to_seq: u64,
    ) -> Vec<Event> {
        (from_seq..=to_seq)
            .filter_map(|seq| {
                self.event_buffer
                    .get(&(bucket.to_string(), origin.to_string(), epoch, seq))
                    .map(|e| e.value().clone())
            })
            .collect()
    }

    pub fn event_is_present(&self, event: &Event) -> bool {
        let key = event.origin_key();
        if let Some(existing) = self.event_buffer.get(&key) {
            return existing.event_id == event.event_id;
        }

        if self
            .pending_seqs
            .get(&event.epoch_key())
            .map(|pending| pending.contains(&event.origin_seq))
            .unwrap_or(false)
        {
            return true;
        }

        let head = self.heads.get(&event.epoch_key()).map(|v| *v).unwrap_or(0);
        event.origin_seq <= head
    }

    // ── Persistence tracking ─────────────────────────────────────────

    pub fn mark_persisted(&self, keys: &[OriginKey]) {
        for key in keys {
            self.unpersisted.remove(key);
            let (bucket, origin, epoch, seq) = key;
            let epoch_key: EpochKey = (bucket.clone(), origin.clone(), *epoch);
            let head = self.heads.get(&epoch_key).map(|v| *v).unwrap_or(0);
            let still_pending = self
                .pending_seqs
                .get(&epoch_key)
                .map(|pending| pending.contains(seq))
                .unwrap_or(false);
            if *seq <= head && !still_pending {
                self.event_buffer.remove(key);
            }
        }
    }

    pub fn get_unpersisted_events(&self, cutoff_ms: u64) -> Vec<Event> {
        self.unpersisted
            .iter()
            .filter(|e| *e.value() <= cutoff_ms)
            .filter_map(|e| self.event_buffer.get(e.key()).map(|v| v.value().clone()))
            .collect()
    }

    pub fn persistence_stats(&self) -> PersistenceStats {
        let now = Event::now_ms();
        let oldest = self
            .unpersisted
            .iter()
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
            if let Some(epoch_set) = self
                .account_origin_epochs
                .get(&(bucket.clone(), account.clone()))
            {
                for (epoch_bucket, origin, epoch) in epoch_set.iter() {
                    // The bucket in the EpochKey is always equal to the
                    // outer `bucket` here (we only index epoch tuples into
                    // a BalanceKey that shares the same bucket), so we
                    // still surface the progress as "origin:epoch" to keep
                    // the collapsed-state public API stable.
                    let _ = epoch_bucket;
                    let key = (bucket.clone(), origin.clone(), *epoch);
                    let head = self.heads.get(&key).map(|v| *v).unwrap_or(0);
                    let max_known = self.max_known_seqs.get(&key).map(|v| *v).unwrap_or(0);
                    origins.insert(
                        format!("{origin}:{epoch}"),
                        OriginProgress { head, max_known },
                    );
                }
            }

            let status = if origins.is_empty() || origins.values().all(|o| o.head >= o.max_known) {
                "locally_confirmed".to_string()
            } else {
                "provisional".to_string()
            };

            result.insert(
                key,
                CollapsedBalance {
                    balance: state.balance,
                    available_balance: state.available_balance(now_ms),
                    status,
                    reserved_by_origin: state.reservations_by_origin(now_ms),
                    contributing_origins: origins,
                },
            );
        }
        result
    }

    // ── Debug (§7.1) ──────────────────────────────────────────────────

    pub fn debug_origin(&self, origin_id: &str) -> DebugOriginResponse {
        // Group every `(bucket, origin=origin_id, epoch)` under its epoch.
        // Multiple buckets can share the same epoch number (they're
        // independent namespaces now), so stored as Vec<_> under each
        // epoch via a transient map.
        type DebugEpochBucketEntry = (String, u64, Vec<u64>, u64);
        let mut per_epoch: BTreeMap<u32, Vec<DebugEpochBucketEntry>> = BTreeMap::new();

        for entry in self.heads.iter() {
            let (bucket, origin, epoch) = entry.key();
            if origin != origin_id {
                continue;
            }
            let head = *entry.value();
            let pending: Vec<u64> = self
                .pending_seqs
                .get(&(bucket.clone(), origin.clone(), *epoch))
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default();
            let max_known = self
                .max_known_seqs
                .get(&(bucket.clone(), origin.clone(), *epoch))
                .map(|v| *v)
                .unwrap_or(head);
            per_epoch
                .entry(*epoch)
                .or_default()
                .push((bucket.clone(), head, pending, max_known));
        }

        let mut epochs = BTreeMap::new();
        for (epoch, entries) in per_epoch {
            // Merge all buckets that share this epoch into a single
            // DebugEpochInfo. head = min(heads) because the debug view is
            // "how advanced are we across this epoch"; max = max(max_known).
            let contiguous_head = entries.iter().map(|e| e.1).min().unwrap_or(0);
            let max_known = entries.iter().map(|e| e.3).max().unwrap_or(0);
            let mut pending: Vec<u64> = entries.into_iter().flat_map(|(_, _, p, _)| p).collect();
            pending.sort();
            pending.dedup();
            let count = contiguous_head as usize + pending.len();
            epochs.insert(
                epoch,
                DebugEpochInfo {
                    contiguous_head,
                    present_seqs: pending,
                    min_seq: if count > 0 { Some(1) } else { None },
                    max_seq: if max_known > 0 { Some(max_known) } else { None },
                    count,
                },
            );
        }

        DebugOriginResponse {
            origin_node_id: origin_id.to_string(),
            epochs,
        }
    }

    // ── Private helpers ──────────────────────────────────────────────

    async fn advance_head(&self, epoch_key: &EpochKey, seq: u64) {
        // Acquire per-epoch lock to make head read + drain_pending + head write atomic.
        // Prevents race where concurrent inserts for seq N+1 and N+2 both read head=N.
        let digest_to_persist = {
            let lock = self
                .head_locks
                .entry(epoch_key.clone())
                .or_insert_with(|| Arc::new(std::sync::Mutex::new(())))
                .value()
                .clone();
            let _guard = lock
                .lock()
                .unwrap_or_else(|e: std::sync::PoisonError<_>| e.into_inner());

            let current = self.heads.get(epoch_key).map(|v| *v).unwrap_or(0);
            if seq == current + 1 {
                let new_head = self.drain_pending(epoch_key, seq);
                // §8.3: Update rolling digest for newly contiguous events [current+1 .. new_head]
                let digest = self.update_digest(epoch_key, current, new_head);
                self.heads.insert(epoch_key.clone(), new_head);
                self.prune_persisted_buffer_range(epoch_key, current + 1, new_head);
                Some((epoch_key.clone(), new_head, digest))
            } else if seq > current + 1 {
                self.pending_seqs
                    .entry(epoch_key.clone())
                    .or_default()
                    .insert(seq);
                self.heads.entry(epoch_key.clone()).or_insert(current);
                None
            } else {
                None
            }
        };

        if let Some(((bucket, origin, epoch), head, digest)) = digest_to_persist
            && let Err(error) = self
                .storage
                .save_digest(&bucket, &origin, epoch, head, &digest)
                .await
        {
            warn!(bucket = %bucket, origin = %origin, epoch, head, error = %error, "failed to persist rolling digest");
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

    /// §8.3: Update rolling prefix digest for events from old_head+1 to new_head.
    /// prefix_digest[n] = SHA256(prefix_digest[n-1] || event_hash(n))
    fn update_digest(&self, epoch_key: &EpochKey, old_head: u64, new_head: u64) -> [u8; 32] {
        let (bucket, origin, epoch) = epoch_key;
        let mut current_digest = self
            .digests
            .get(epoch_key)
            .map(|d| d.1)
            .unwrap_or([0u8; 32]); // prefix_digest[0] = zeroed

        for seq in (old_head + 1)..=new_head {
            let origin_key: OriginKey = (bucket.clone(), origin.clone(), *epoch, seq);
            if let Some(event) = self.event_buffer.get(&origin_key) {
                let event_hash = Sha256::digest(event.canonical().as_bytes());
                let mut hasher = Sha256::new();
                hasher.update(current_digest);
                hasher.update(event_hash);
                current_digest = hasher.finalize().into();
            }
        }

        self.digests
            .insert(epoch_key.clone(), (new_head, current_digest));
        current_digest
    }

    fn prune_persisted_buffer_range(&self, epoch_key: &EpochKey, from_seq: u64, to_seq: u64) {
        if from_seq > to_seq {
            return;
        }

        let (bucket, origin, epoch) = epoch_key;
        for seq in from_seq..=to_seq {
            let key: OriginKey = (bucket.clone(), origin.clone(), *epoch, seq);
            if !self.unpersisted.contains_key(&key) {
                self.event_buffer.remove(&key);
            }
        }
    }

    fn update_origin_tracking(&self, event: &Event) {
        self.account_origin_epochs
            .entry(event.balance_key())
            .or_default()
            .insert(event.epoch_key());
        self.max_known_seqs
            .entry(event.epoch_key())
            .and_modify(|max| {
                if event.origin_seq > *max {
                    *max = event.origin_seq;
                }
            })
            .or_insert(event.origin_seq);
    }

    async fn record_new_local_event(&self, event: &Event) {
        self.store_event_buffer(event);
        self.advance_head(&event.epoch_key(), event.origin_seq)
            .await;
        self.update_origin_tracking(event);
        self.total_event_count.fetch_add(1, Relaxed);
        let _ = self.batch_tx.send(event.clone());
    }

    fn store_event_buffer(&self, event: &Event) {
        let key = event.origin_key();
        self.event_buffer.insert(key.clone(), event.clone());
        self.unpersisted.insert(key, event.created_at_unix_ms);
    }
}

// ── UnpersistedSource impl for OrphanDetector ────────────────────────

impl<S: shardd_storage::StorageBackend> crate::orphan_detector::UnpersistedSource
    for SharedState<S>
{
    fn get_unpersisted_events(&self, cutoff_ms: u64) -> Vec<Event> {
        SharedState::get_unpersisted_events(self, cutoff_ms)
    }

    fn mark_persisted(&self, keys: &[OriginKey]) {
        SharedState::mark_persisted(self, keys);
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

#[cfg(test)]
mod tests {
    use super::*;
    use shardd_storage::StorageBackend;
    use shardd_storage::memory::InMemoryStorage;

    async fn make_state() -> SharedState<InMemoryStorage> {
        let storage = InMemoryStorage::new();
        storage
            .save_node_meta(&NodeMeta {
                node_id: "test-node".into(),
                host: "127.0.0.1".into(),
                port: 0,
            })
            .await
            .unwrap();

        let (tx, _rx) = mpsc::unbounded_channel();
        let (ctx, _crx) = mpsc::unbounded_channel();
        SharedState::new(
            "test-node".into(),
            "127.0.0.1:3000".into(),
            storage,
            tx,
            ctx,
            HoldConfig {
                multiplier: 0,
                duration_ms: 0,
            },
        )
        .await
    }

    async fn make_state_with_holds() -> SharedState<InMemoryStorage> {
        let storage = InMemoryStorage::new();
        storage
            .save_node_meta(&NodeMeta {
                node_id: "test-node".into(),
                host: "127.0.0.1".into(),
                port: 0,
            })
            .await
            .unwrap();

        let (tx, _rx) = mpsc::unbounded_channel();
        let (ctx, _crx) = mpsc::unbounded_channel();
        // hold_multiplier=5, hold_duration=600000ms (10 min)
        SharedState::new(
            "test-node".into(),
            "127.0.0.1:3000".into(),
            storage,
            tx,
            ctx,
            HoldConfig {
                multiplier: 5,
                duration_ms: 600_000,
            },
        )
        .await
    }

    #[tokio::test]
    async fn create_event_increments_seq() {
        let state = make_state().await;
        let e1 = state
            .create_local_event(
                "b".into(),
                "a".into(),
                100,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();
        let e2 = state
            .create_local_event(
                "b".into(),
                "a".into(),
                50,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();
        assert_eq!(e1.origin_seq, 1);
        assert_eq!(e2.origin_seq, 2);
        assert_eq!(e1.origin_epoch, 1);
    }

    #[tokio::test]
    async fn overdraft_guard_rejects() {
        let state = make_state().await;
        state
            .create_local_event(
                "b".into(),
                "a".into(),
                100,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();
        let result = state
            .create_local_event(
                "b".into(),
                "a".into(),
                -200,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(state.account_balance("b", "a"), 100); // unchanged
    }

    #[tokio::test]
    async fn overdraft_guard_with_limit() {
        let state = make_state().await;
        state
            .create_local_event(
                "b".into(),
                "a".into(),
                100,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();
        let result = state
            .create_local_event(
                "b".into(),
                "a".into(),
                -200,
                None,
                200,
                uuid::Uuid::new_v4().to_string(),
            )
            .await;
        assert!(result.is_ok());
        assert_eq!(state.account_balance("b", "a"), -100);
    }

    #[tokio::test]
    async fn replicated_event_bypass_overdraft() {
        let state = make_state().await;
        let event = Event {
            event_id: "remote-1".into(),
            origin_node_id: "remote".into(),
            origin_epoch: 1,
            origin_seq: 1,
            created_at_unix_ms: 1000,
            r#type: EventType::Standard,
            bucket: "b".into(),
            account: "a".into(),
            amount: -999,
            note: None,
            idempotency_nonce: uuid::Uuid::new_v4().to_string(),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };
        assert!(state.insert_event(&event).await);
        assert_eq!(state.account_balance("b", "a"), -999);
    }

    #[tokio::test]
    async fn replication_dedup() {
        let state = make_state().await;
        let event = Event {
            event_id: "e1".into(),
            origin_node_id: "n1".into(),
            origin_epoch: 1,
            origin_seq: 1,
            created_at_unix_ms: 1000,
            r#type: EventType::Standard,
            bucket: "b".into(),
            account: "a".into(),
            amount: 100,
            note: None,
            idempotency_nonce: uuid::Uuid::new_v4().to_string(),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };
        assert!(state.insert_event(&event).await);
        assert!(!state.insert_event(&event).await); // duplicate
        assert_eq!(state.account_balance("b", "a"), 100); // not 200
    }

    #[tokio::test]
    async fn head_advancement_with_gap_fill() {
        let state = make_state().await;
        let make = |seq: u64| Event {
            event_id: format!("e{seq}"),
            origin_node_id: "n1".into(),
            origin_epoch: 1,
            origin_seq: seq,
            created_at_unix_ms: seq * 1000,
            r#type: EventType::Standard,
            bucket: "b".into(),
            account: "a".into(),
            amount: 1,
            note: None,
            idempotency_nonce: uuid::Uuid::new_v4().to_string(),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };

        state.insert_event(&make(1)).await;
        state.insert_event(&make(3)).await; // gap at 2
        let heads = state.get_heads();
        assert_eq!(heads.get("b\tn1:1"), Some(&1)); // stuck at 1

        state.insert_event(&make(2)).await; // fill gap
        let heads = state.get_heads();
        assert_eq!(heads.get("b\tn1:1"), Some(&3)); // advanced to 3
    }

    #[tokio::test]
    async fn epoch_aware_heads() {
        let state = make_state().await;
        let make = |epoch: u32, seq: u64| Event {
            event_id: format!("e{epoch}-{seq}"),
            origin_node_id: "n1".into(),
            origin_epoch: epoch,
            origin_seq: seq,
            created_at_unix_ms: 1000,
            r#type: EventType::Standard,
            bucket: "b".into(),
            account: "a".into(),
            amount: 1,
            note: None,
            idempotency_nonce: uuid::Uuid::new_v4().to_string(),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };

        state.insert_event(&make(1, 1)).await;
        state.insert_event(&make(1, 2)).await;
        state.insert_event(&make(2, 1)).await; // different epoch

        let heads = state.get_heads();
        assert_eq!(heads.get("b\tn1:1"), Some(&2));
        assert_eq!(heads.get("b\tn1:2"), Some(&1));
    }

    #[tokio::test]
    async fn idempotency_local_dedup() {
        let state = make_state().await;
        state
            .create_local_event(
                "b".into(),
                "a".into(),
                100,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();

        let e1 = state
            .create_local_event("b".into(), "a".into(), -50, None, 0, "nonce1".to_string())
            .await
            .unwrap();
        let e2 = state
            .create_local_event("b".into(), "a".into(), -50, None, 0, "nonce1".to_string())
            .await
            .unwrap();

        assert_eq!(e1.event_id, e2.event_id); // same event returned
        assert_eq!(state.account_balance("b", "a"), 50); // charged once, not twice
    }

    #[tokio::test]
    async fn available_balance_with_holds() {
        // Use state with hold_multiplier=5, so a -100 debit requests 500 reserved units
        let state = make_state_with_holds().await;
        state
            .create_local_event(
                "b".into(),
                "a".into(),
                1000,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();
        state
            .create_local_event(
                "b".into(),
                "a".into(),
                -100,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();

        assert_eq!(state.account_balance("b", "a"), 900); // settled
        assert_eq!(state.account_available_balance("b", "a"), 400); // 900 - 500 hold (100 * 5)
    }

    #[tokio::test]
    async fn repeated_local_debits_only_top_up_hold_shortfall() {
        let state = make_state_with_holds().await;
        state
            .create_local_event(
                "b".into(),
                "a".into(),
                1000,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();

        let first = state
            .create_local_events(LocalCreateInput::legacy(
                "b".into(),
                "a".into(),
                -100,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
                false,
            ))
            .await
            .unwrap();
        let second = state
            .create_local_events(LocalCreateInput::legacy(
                "b".into(),
                "a".into(),
                -100,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
                false,
            ))
            .await
            .unwrap();

        assert_eq!(first.emitted_events.len(), 2);
        assert_eq!(first.emitted_events[0].r#type, EventType::ReservationCreate);
        assert_eq!(first.emitted_events[1].r#type, EventType::Standard);
        assert_eq!(second.emitted_events.len(), 1);
        assert_eq!(second.emitted_events[0].r#type, EventType::Standard);
        assert_eq!(state.account_balance("b", "a"), 800);
        assert_eq!(state.account_available_balance("b", "a"), 300);
        let balances = state.get_all_balances();
        let account = balances
            .iter()
            .find(|balance| balance.bucket == "b" && balance.account == "a")
            .unwrap();
        assert_eq!(account.reserved_by_origin["test-node"].reserved_amount, 500);
        assert_eq!(account.reserved_by_origin["test-node"].reservation_count, 1);
    }

    #[tokio::test]
    async fn larger_followup_debit_only_reserves_hold_shortfall() {
        let state = make_state_with_holds().await;
        state
            .create_local_event(
                "b".into(),
                "a".into(),
                1000,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();

        let first = state
            .create_local_events(LocalCreateInput::legacy(
                "b".into(),
                "a".into(),
                -100,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
                false,
            ))
            .await
            .unwrap();
        let second = state
            .create_local_events(LocalCreateInput::legacy(
                "b".into(),
                "a".into(),
                -150,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
                false,
            ))
            .await
            .unwrap();

        assert_eq!(first.emitted_events.len(), 2);
        assert_eq!(first.emitted_events[0].hold_amount, 500);
        assert_eq!(second.emitted_events.len(), 2);
        assert_eq!(second.emitted_events[0].hold_amount, 250);
        assert_eq!(state.account_balance("b", "a"), 750);
        assert_eq!(state.account_available_balance("b", "a"), 0);
        let balances = state.get_all_balances();
        let account = balances
            .iter()
            .find(|balance| balance.bucket == "b" && balance.account == "a")
            .unwrap();
        assert_eq!(account.reserved_by_origin["test-node"].reserved_amount, 750);
        assert_eq!(account.reserved_by_origin["test-node"].reservation_count, 2);
    }

    #[tokio::test]
    async fn remote_reservations_do_not_satisfy_local_shortfall() {
        let state = make_state_with_holds().await;
        state
            .create_local_event(
                "b".into(),
                "a".into(),
                2000,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();

        let now_ms = Event::now_ms();
        let remote = Event {
            event_id: "remote-hold".into(),
            origin_node_id: "remote".into(),
            origin_epoch: 1,
            origin_seq: 1,
            created_at_unix_ms: now_ms,
            r#type: EventType::ReservationCreate,
            bucket: "b".into(),
            account: "a".into(),
            amount: 0,
            note: None,
            idempotency_nonce: uuid::Uuid::new_v4().to_string(),
            void_ref: None,
            hold_amount: 500,
            hold_expires_at_unix_ms: now_ms + 600_000,
        };
        assert!(state.insert_event(&remote).await);

        let local = state
            .create_local_events(LocalCreateInput::legacy(
                "b".into(),
                "a".into(),
                -100,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
                false,
            ))
            .await
            .unwrap();
        assert_eq!(local.emitted_events.len(), 2);
        assert_eq!(local.emitted_events[0].r#type, EventType::ReservationCreate);
        assert_eq!(local.emitted_events[0].hold_amount, 500);
        assert_eq!(local.primary_event.hold_amount, 0);

        let balances = state.get_all_balances();
        let account = balances
            .iter()
            .find(|balance| balance.bucket == "b" && balance.account == "a")
            .unwrap();
        assert_eq!(account.active_hold_total, 1000);
        assert_eq!(account.reserved_by_origin["remote"].reserved_amount, 500);
        assert_eq!(account.reserved_by_origin["remote"].reservation_count, 1);
        assert!(
            account.reserved_by_origin["remote"]
                .next_expiry_unix_ms
                .is_some()
        );
        assert_eq!(account.reserved_by_origin["test-node"].reserved_amount, 500);
        assert_eq!(account.reserved_by_origin["test-node"].reservation_count, 1);

        let collapsed = state.collapsed_state();
        assert_eq!(
            collapsed["b:a"].reserved_by_origin["remote"].reserved_amount,
            500
        );
        assert_eq!(
            collapsed["b:a"].reserved_by_origin["test-node"].reserved_amount,
            500
        );
    }

    #[tokio::test]
    async fn near_expiry_local_reservation_is_renewed() {
        let state = make_state_with_holds().await;
        state
            .create_local_event(
                "b".into(),
                "a".into(),
                2000,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();

        let now_ms = Event::now_ms();
        let local_expiring = Event {
            event_id: "local-expiring-hold".into(),
            origin_node_id: "test-node".into(),
            origin_epoch: 99,
            origin_seq: 1,
            created_at_unix_ms: now_ms,
            r#type: EventType::ReservationCreate,
            bucket: "b".into(),
            account: "a".into(),
            amount: 0,
            note: None,
            idempotency_nonce: uuid::Uuid::new_v4().to_string(),
            void_ref: None,
            hold_amount: 500,
            hold_expires_at_unix_ms: now_ms + 59_000,
        };
        assert!(state.insert_event(&local_expiring).await);

        let renewed = state
            .create_local_events(LocalCreateInput::legacy(
                "b".into(),
                "a".into(),
                -100,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
                false,
            ))
            .await
            .unwrap();
        assert_eq!(renewed.emitted_events.len(), 2);
        assert_eq!(
            renewed.emitted_events[0].r#type,
            EventType::ReservationCreate
        );
        assert_eq!(renewed.emitted_events[0].hold_amount, 500);
        assert_eq!(renewed.primary_event.hold_amount, 0);

        let balances = state.get_all_balances();
        let account = balances
            .iter()
            .find(|balance| balance.bucket == "b" && balance.account == "a")
            .unwrap();
        assert_eq!(
            account.reserved_by_origin["test-node"].reserved_amount,
            1000
        );
        assert_eq!(account.reserved_by_origin["test-node"].reservation_count, 2);
    }

    #[tokio::test]
    async fn collapsed_state_confirmed_vs_provisional() {
        let state = make_state().await;
        state
            .create_local_event(
                "b".into(),
                "a".into(),
                100,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();

        let collapsed = state.collapsed_state();
        assert_eq!(collapsed["b:a"].status, "locally_confirmed");

        // Add remote events with a gap
        let make = |seq: u64| Event {
            event_id: format!("r{seq}"),
            origin_node_id: "remote".into(),
            origin_epoch: 1,
            origin_seq: seq,
            created_at_unix_ms: 1000,
            r#type: EventType::Standard,
            bucket: "b".into(),
            account: "a".into(),
            amount: 10,
            note: None,
            idempotency_nonce: uuid::Uuid::new_v4().to_string(),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
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
            event_id: "e1".into(),
            origin_node_id: "n1".into(),
            origin_epoch: 1,
            origin_seq: 1,
            created_at_unix_ms: 1000,
            r#type: EventType::Standard,
            bucket: "b".into(),
            account: "a".into(),
            amount: 100,
            note: None,
            idempotency_nonce: uuid::Uuid::new_v4().to_string(),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };
        state.insert_event(&event).await;

        assert_eq!(state.persistence_stats().unpersisted, 1);
        state.mark_persisted(&[("b".into(), "n1".into(), 1, 1)]);
        assert_eq!(state.persistence_stats().unpersisted, 0);
        assert!(state.get_events_from_buffer("b", "n1", 1, 1, 1).is_empty());
    }

    #[tokio::test]
    async fn restart_rebuilds_pending_gap_state_and_dedups_replay() {
        let storage = InMemoryStorage::new();
        storage
            .save_node_meta(&NodeMeta {
                node_id: "test-node".into(),
                host: "127.0.0.1".into(),
                port: 0,
            })
            .await
            .unwrap();

        let e1 = Event {
            event_id: "remote-1".into(),
            origin_node_id: "remote".into(),
            origin_epoch: 1,
            origin_seq: 1,
            created_at_unix_ms: 1000,
            r#type: EventType::Standard,
            bucket: "b".into(),
            account: "a".into(),
            amount: 10,
            note: None,
            idempotency_nonce: uuid::Uuid::new_v4().to_string(),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };
        let e3 = Event {
            event_id: "remote-3".into(),
            origin_node_id: "remote".into(),
            origin_epoch: 1,
            origin_seq: 3,
            created_at_unix_ms: 3000,
            r#type: EventType::Standard,
            bucket: "b".into(),
            account: "a".into(),
            amount: 30,
            note: None,
            idempotency_nonce: uuid::Uuid::new_v4().to_string(),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };
        storage.insert_event(&e1).await.unwrap();
        storage.insert_event(&e3).await.unwrap();

        let (tx, _rx) = mpsc::unbounded_channel();
        let (ctx, _crx) = mpsc::unbounded_channel();
        let state = SharedState::new(
            "test-node".into(),
            "127.0.0.1:3000".into(),
            storage,
            tx,
            ctx,
            HoldConfig {
                multiplier: 0,
                duration_ms: 0,
            },
        )
        .await;

        let heads = state.get_heads();
        assert_eq!(heads.get("b\tremote:1"), Some(&1));
        assert_eq!(
            state.get_events_from_buffer("b", "remote", 1, 3, 3).len(),
            1
        );

        assert!(
            !state.insert_event(&e3).await,
            "persisted pending event should dedup after restart"
        );
        assert_eq!(state.account_balance("b", "a"), 40);
    }

    #[tokio::test]
    async fn local_event_updates_rolling_digest() {
        let state = make_state().await;
        let event = state
            .create_local_event(
                "b".into(),
                "a".into(),
                10,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();

        let mut expected_digest = [0u8; 32];
        let event_hash = Sha256::digest(event.canonical().as_bytes());
        let mut hasher = Sha256::new();
        hasher.update(expected_digest);
        hasher.update(event_hash);
        expected_digest = hasher.finalize().into();

        let digests = state.get_digests();
        let key = format!(
            "{}\t{}:{}",
            event.bucket, event.origin_node_id, event.origin_epoch
        );
        let (head, hex_digest) = digests
            .get(&key)
            .expect("digest should exist for local event");
        assert_eq!(*head, 1);
        let expected_hex: String = expected_digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(*hex_digest, expected_hex);
    }

    #[tokio::test]
    async fn idempotency_db_fallback_after_cache_miss() {
        // Create a state, insert event with nonce, then manually write it to storage
        // and clear the in-memory cache to simulate a restart scenario
        let storage = InMemoryStorage::new();
        storage
            .save_node_meta(&NodeMeta {
                node_id: "test-node".into(),
                host: "127.0.0.1".into(),
                port: 0,
            })
            .await
            .unwrap();

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
            idempotency_nonce: "nonce-db-test".to_string(),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };
        storage.insert_event(&prior_event).await.unwrap();

        // Create state — idempotency cache is empty (not rebuilt from DB)
        let (tx, _rx) = mpsc::unbounded_channel();
        let (ctx, _crx) = mpsc::unbounded_channel();
        let state = SharedState::new(
            "test-node".into(),
            "127.0.0.1:3000".into(),
            storage,
            tx,
            ctx,
            HoldConfig {
                multiplier: 0,
                duration_ms: 0,
            },
        )
        .await;

        // Fund the account so overdraft doesn't reject
        state
            .create_local_event(
                "b".into(),
                "a".into(),
                1000,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();

        // Now try to create with the same nonce — should hit DB fallback and dedup
        let result = state
            .create_local_event(
                "b".into(),
                "a".into(),
                -50,
                None,
                0,
                "nonce-db-test".to_string(),
            )
            .await
            .unwrap();

        // Should return the prior event (dedup from DB)
        assert_eq!(result.event_id, "prior-event-123");
        // Balance should NOT have been charged again
        assert_eq!(state.account_balance("b", "a"), 950); // 1000 - 50 from rebuild, not -50 again
    }

    #[tokio::test]
    async fn cross_node_idempotency_conflict_emits_void() {
        let state = make_state().await;
        state
            .create_local_event(
                "b".into(),
                "a".into(),
                1000,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();

        // This node creates an event with a nonce
        let local = state
            .create_local_event(
                "b".into(),
                "a".into(),
                -50,
                None,
                0,
                "completion:abc".to_string(),
            )
            .await
            .unwrap();

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
            idempotency_nonce: "completion:abc".to_string(),
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
        state
            .create_local_event(
                "b".into(),
                "a".into(),
                1000,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();

        let e1 = state
            .create_local_event("b".into(), "a".into(), -50, None, 0, "nonce1".to_string())
            .await
            .unwrap();
        let e2 = state
            .create_local_event("b".into(), "a".into(), -100, None, 0, "nonce1".to_string())
            .await
            .unwrap();

        // Different amounts = different operations, not duplicates
        assert_ne!(e1.event_id, e2.event_id);
        assert_eq!(state.account_balance("b", "a"), 850); // 1000 - 50 - 100
    }

    #[tokio::test]
    async fn concurrent_inserts_advance_head_correctly() {
        let state = Arc::new(make_state().await);
        let origin = "remote-node".to_string();
        let epoch = 1u32;

        // Create 100 events for a remote origin and insert them concurrently
        let mut events: Vec<Event> = (1..=100)
            .map(|seq| Event {
                event_id: Event::generate_id(),
                origin_node_id: origin.clone(),
                origin_epoch: epoch,
                origin_seq: seq,
                created_at_unix_ms: Event::now_ms(),
                r#type: EventType::Standard,
                bucket: "b".into(),
                account: "a".into(),
                amount: 1,
                note: None,
                idempotency_nonce: uuid::Uuid::new_v4().to_string(),
                void_ref: None,
                hold_amount: 0,
                hold_expires_at_unix_ms: 0,
            })
            .collect();

        // Shuffle to ensure out-of-order arrival
        use rand::seq::SliceRandom;
        let mut rng = rand::rng();
        events.shuffle(&mut rng);

        // Insert all concurrently using tokio tasks
        let mut handles = Vec::new();
        for event in events {
            let s = state.clone();
            handles.push(tokio::spawn(async move {
                s.insert_event(&event).await;
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        // Head must be 100 — all events 1..=100 are present
        let heads = state.get_heads();
        let key = format!("b\t{origin}:{epoch}");
        assert_eq!(
            heads.get(&key).copied().unwrap_or(0),
            100,
            "head should be 100 after concurrent insertion of events 1-100"
        );
    }

    #[tokio::test]
    async fn rolling_digest_computed_on_head_advance() {
        let state = make_state().await;
        let origin = "remote".to_string();
        let epoch = 1u32;

        // Insert events 1-5 in order
        let mut expected_digest = [0u8; 32]; // prefix_digest[0] = zeroed
        for seq in 1..=5u64 {
            let event = Event {
                event_id: Event::generate_id(),
                origin_node_id: origin.clone(),
                origin_epoch: epoch,
                origin_seq: seq,
                created_at_unix_ms: 1000 + seq,
                r#type: EventType::Standard,
                bucket: "b".into(),
                account: "a".into(),
                amount: 10,
                note: None,
                idempotency_nonce: uuid::Uuid::new_v4().to_string(),
                void_ref: None,
                hold_amount: 0,
                hold_expires_at_unix_ms: 0,
            };

            // Manually compute expected digest
            use sha2::{Digest as _, Sha256};
            let event_hash = Sha256::digest(event.canonical().as_bytes());
            let mut hasher = Sha256::new();
            hasher.update(expected_digest);
            hasher.update(event_hash);
            expected_digest = hasher.finalize().into();

            state.insert_event(&event).await;
        }

        let digests = state.get_digests();
        let key = format!("b\t{origin}:{epoch}");
        let (head, hex_digest) = digests.get(&key).expect("digest should exist");
        assert_eq!(*head, 5);
        let expected_hex: String = expected_digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            *hex_digest, expected_hex,
            "rolling digest should match manual computation"
        );
    }

    #[tokio::test]
    async fn bucket_delete_cascade_removes_all_state() {
        let state = make_state().await;
        // Populate buckets A, B, C.
        state
            .create_local_event(
                "A".into(),
                "main".into(),
                100,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();
        state
            .create_local_event(
                "A".into(),
                "alt".into(),
                50,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();
        state
            .create_local_event(
                "B".into(),
                "main".into(),
                200,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();
        state
            .create_local_event(
                "C".into(),
                "main".into(),
                300,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();

        // Trigger a BucketDelete for B via the local meta path.
        state
            .create_meta_bucket_delete("B", Some("test".into()))
            .await
            .unwrap();

        // B is tombstoned.
        assert!(state.deleted_buckets.contains_key("B"));
        assert!(!state.deleted_buckets.contains_key("A"));
        assert!(!state.deleted_buckets.contains_key("C"));

        // B's in-memory state is gone.
        assert!(state.heads.iter().all(|e| {
            let (b, _, _) = e.key();
            b != "B"
        }));
        assert!(state.max_known_seqs.iter().all(|e| {
            let (b, _, _) = e.key();
            b != "B"
        }));
        assert!(state.event_buffer.iter().all(|e| {
            let (b, _, _, _) = e.key();
            b != "B"
        }));
        assert_eq!(state.account_balance("B", "main"), 0);

        // A and C untouched.
        assert_eq!(state.account_balance("A", "main"), 100);
        assert_eq!(state.account_balance("A", "alt"), 50);
        assert_eq!(state.account_balance("C", "main"), 300);

        // Meta event lives on.
        assert!(state.heads.iter().any(|e| {
            let (b, _, _) = e.key();
            b == META_BUCKET
        }));
    }

    #[tokio::test]
    async fn write_to_deleted_bucket_is_rejected() {
        let state = make_state().await;
        state
            .create_local_event(
                "doomed".into(),
                "main".into(),
                50,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();
        state
            .create_meta_bucket_delete("doomed", None)
            .await
            .unwrap();

        let err = state
            .create_local_event(
                "doomed".into(),
                "main".into(),
                10,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap_err();
        matches!(err, CreateLocalEventError::BucketDeleted(_));
    }

    #[tokio::test]
    async fn reserved_bucket_names_are_rejected() {
        let state = make_state().await;
        let err = state
            .create_local_event(
                META_BUCKET.into(),
                "main".into(),
                1,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap_err();
        matches!(err, CreateLocalEventError::BucketReserved(_));

        let err = state
            .create_local_event(
                "__billing__abc".into(),
                "main".into(),
                1,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap_err();
        matches!(err, CreateLocalEventError::BucketReserved(_));

        // Meta path refuses to tombstone itself or any reserved name.
        let err = state
            .create_meta_bucket_delete(META_BUCKET, None)
            .await
            .unwrap_err();
        matches!(err, CreateLocalEventError::BucketReserved(_));
    }

    #[tokio::test]
    async fn restart_preserves_deleted_buckets() {
        // `InMemoryStorage` isn't Clone, so we can't share one across
        // two `SharedState::new` calls. Simulate a restart by pre-
        // populating storage with a BucketDelete meta event and then
        // constructing a fresh SharedState — the replay in
        // SharedState::new should rebuild `deleted_buckets`.
        let storage = InMemoryStorage::new();
        storage
            .save_node_meta(&NodeMeta {
                node_id: "test-node".into(),
                host: "127.0.0.1".into(),
                port: 0,
            })
            .await
            .unwrap();
        let meta_delete = Event {
            event_id: "meta-1".into(),
            origin_node_id: "test-node".into(),
            origin_epoch: 1,
            origin_seq: 1,
            created_at_unix_ms: 1_700_000_000_000,
            r#type: EventType::BucketDelete,
            bucket: META_BUCKET.to_string(),
            account: "gone".into(),
            amount: 0,
            note: Some("restart-test".into()),
            idempotency_nonce: uuid::Uuid::new_v4().to_string(),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };
        storage.insert_event(&meta_delete).await.unwrap();

        let (tx, _rx) = mpsc::unbounded_channel();
        let (ctx, _crx) = mpsc::unbounded_channel();
        let restarted = SharedState::new(
            "test-node".into(),
            "127.0.0.1:3000".into(),
            storage,
            tx,
            ctx,
            HoldConfig {
                multiplier: 0,
                duration_ms: 0,
            },
        )
        .await;
        assert!(restarted.deleted_buckets.contains_key("gone"));
        let err = restarted
            .create_local_event(
                "gone".into(),
                "main".into(),
                1,
                None,
                0,
                uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap_err();
        matches!(err, CreateLocalEventError::BucketDeleted(_));
    }
}
