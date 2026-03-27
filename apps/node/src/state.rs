use dashmap::DashMap;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use shardd_storage::Storage;
use shardd_types::{Event, NodeMeta, PeersFile};

use crate::peer::PeerSet;

/// Per-origin event store: events + contiguous head bundled together.
/// Accessed via DashMap per-shard lock — no global contention.
pub struct OriginState {
    pub events: BTreeMap<u64, Event>,
    pub contiguous_head: u64,
}

impl OriginState {
    fn recompute_head(&mut self) {
        let mut head = self.contiguous_head;
        while self.events.contains_key(&(head + 1)) {
            head += 1;
        }
        self.contiguous_head = head;
    }
}

enum PersistOp {
    AppendEvent(Event),
    SaveMeta(NodeMeta),
    SavePeers(PeersFile),
}

/// Shared state with granular, per-field locking.
///
/// - `node_id`, `addr`: immutable, zero contention
/// - `next_seq`: atomic, lock-free
/// - `peers`: separate Mutex, doesn't block event operations
/// - `origins`: DashMap sharded by origin_node_id, per-origin lock
/// - `event_count`, `balance`: atomics for O(1) /health reads
#[derive(Clone)]
pub struct SharedState {
    pub node_id: Arc<str>,
    pub addr: Arc<str>,
    pub next_seq: Arc<AtomicU64>,
    pub peers: Arc<Mutex<PeerSet>>,
    pub origins: Arc<DashMap<String, OriginState>>,
    pub event_count: Arc<AtomicUsize>,
    pub balance: Arc<AtomicI64>,
    persist_tx: mpsc::UnboundedSender<PersistOp>,
}

impl SharedState {
    pub fn new(
        node_id: String,
        addr: String,
        next_seq: u64,
        peers: PeerSet,
        events_by_origin: BTreeMap<String, BTreeMap<u64, Event>>,
        storage: Storage,
    ) -> Self {
        let origins = DashMap::new();
        let mut total_events = 0usize;
        let mut total_balance = 0i64;

        for (origin, events) in events_by_origin {
            total_events += events.len();
            total_balance += events.values().map(|e| e.amount).sum::<i64>();
            let mut state = OriginState {
                events,
                contiguous_head: 0,
            };
            state.recompute_head();
            origins.insert(origin, state);
        }

        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(persist_loop(storage, rx));

        Self {
            node_id: Arc::from(node_id.as_str()),
            addr: Arc::from(addr.as_str()),
            next_seq: Arc::new(AtomicU64::new(next_seq)),
            peers: Arc::new(Mutex::new(peers)),
            origins: Arc::new(origins),
            event_count: Arc::new(AtomicUsize::new(total_events)),
            balance: Arc::new(AtomicI64::new(total_balance)),
            persist_tx: tx,
        }
    }

    /// Insert an event idempotently. Returns true if new.
    /// Only locks the specific origin shard — other origins unaffected.
    pub fn insert_event(&self, event: Event) -> bool {
        let origin = event.origin_node_id.clone();
        let seq = event.origin_seq;
        let amount = event.amount;

        let mut entry = self.origins.entry(origin).or_insert_with(|| OriginState {
            events: BTreeMap::new(),
            contiguous_head: 0,
        });

        if entry.events.contains_key(&seq) {
            return false;
        }

        entry.events.insert(seq, event.clone());
        entry.recompute_head();
        drop(entry); // release shard lock

        self.event_count.fetch_add(1, Relaxed);
        self.balance.fetch_add(amount, Relaxed);
        let _ = self.persist_tx.send(PersistOp::AppendEvent(event));
        true
    }

    /// Insert multiple events, grouped by origin for minimal shard contention.
    pub fn insert_events_batch(&self, events: Vec<Event>) -> usize {
        // Group by origin to minimize shard lock acquisitions
        let mut by_origin: BTreeMap<String, Vec<Event>> = BTreeMap::new();
        for event in events {
            by_origin
                .entry(event.origin_node_id.clone())
                .or_default()
                .push(event);
        }

        let mut inserted = 0usize;
        let mut balance_delta = 0i64;
        let mut to_persist = Vec::new();

        for (origin, events) in by_origin {
            let mut entry = self.origins.entry(origin).or_insert_with(|| OriginState {
                events: BTreeMap::new(),
                contiguous_head: 0,
            });

            for event in events {
                if !entry.events.contains_key(&event.origin_seq) {
                    balance_delta += event.amount;
                    entry.events.insert(event.origin_seq, event.clone());
                    to_persist.push(event);
                    inserted += 1;
                }
            }
            entry.recompute_head();
        }

        if inserted > 0 {
            self.event_count.fetch_add(inserted, Relaxed);
            self.balance.fetch_add(balance_delta, Relaxed);
            for event in to_persist {
                let _ = self.persist_tx.send(PersistOp::AppendEvent(event));
            }
        }
        inserted
    }

    /// Create a new local event. Lock-free seq allocation via atomic.
    pub fn create_local_event(&self, amount: i64, note: Option<String>) -> Event {
        let seq = self.next_seq.fetch_add(1, Relaxed);
        let event = Event {
            event_id: uuid::Uuid::new_v4().to_string(),
            origin_node_id: self.node_id.to_string(),
            origin_seq: seq,
            created_at_unix_ms: now_ms(),
            amount,
            note,
        };

        self.insert_event(event.clone());

        // Persist updated next_seq
        let _ = self.persist_tx.send(PersistOp::SaveMeta(NodeMeta {
            node_id: self.node_id.to_string(),
            host: self
                .addr
                .split(':')
                .next()
                .unwrap_or("127.0.0.1")
                .to_string(),
            port: self
                .addr
                .split(':')
                .nth(1)
                .and_then(|p| p.parse().ok())
                .unwrap_or(0),
            next_seq: seq + 1,
        }));

        event
    }

    pub fn event_count(&self) -> usize {
        self.event_count.load(Relaxed)
    }

    pub fn balance(&self) -> i64 {
        self.balance.load(Relaxed)
    }

    /// Collect contiguous heads from all origins.
    pub fn get_heads(&self) -> BTreeMap<String, u64> {
        self.origins
            .iter()
            .map(|entry| (entry.key().clone(), entry.contiguous_head))
            .collect()
    }

    /// Get events in [from_seq, to_seq] for a single origin.
    pub fn get_events_range(&self, origin: &str, from_seq: u64, to_seq: u64) -> Vec<Event> {
        let Some(entry) = self.origins.get(origin) else {
            return vec![];
        };
        entry
            .events
            .range(from_seq..=to_seq)
            .map(|(_, e)| e.clone())
            .collect()
    }

    /// Deterministic checksum across all origins.
    pub fn checksum(&self) -> String {
        // Collect and sort by origin for determinism
        let mut all: Vec<(String, Vec<(u64, Event)>)> = self
            .origins
            .iter()
            .map(|entry| {
                let origin = entry.key().clone();
                let events: Vec<(u64, Event)> = entry
                    .events
                    .iter()
                    .map(|(seq, e)| (*seq, e.clone()))
                    .collect();
                (origin, events)
            })
            .collect();
        all.sort_by(|a, b| a.0.cmp(&b.0));

        let mut hasher = Sha256::new();
        for (origin, events) in &all {
            for (seq, event) in events {
                hasher.update(format!(
                    "{}:{}:{}:{}:{}\n",
                    origin,
                    seq,
                    event.event_id,
                    event.amount,
                    event.note.as_deref().unwrap_or("")
                ));
            }
        }
        format!("{:x}", hasher.finalize())
    }

    /// All events sorted for presentation.
    pub fn all_events_sorted(&self) -> Vec<Event> {
        let mut events: Vec<Event> = self
            .origins
            .iter()
            .flat_map(|entry| entry.events.values().cloned().collect::<Vec<_>>())
            .collect();
        events.sort_by(|a, b| {
            a.created_at_unix_ms
                .cmp(&b.created_at_unix_ms)
                .then_with(|| a.origin_node_id.cmp(&b.origin_node_id))
                .then_with(|| a.origin_seq.cmp(&b.origin_seq))
        });
        events
    }

    pub async fn persist_peers(&self) {
        let pf = {
            let peers = self.peers.lock().await;
            PeersFile {
                peers: peers.to_vec(),
            }
        };
        let _ = self.persist_tx.send(PersistOp::SavePeers(pf));
    }
}

async fn persist_loop(storage: Storage, mut rx: mpsc::UnboundedReceiver<PersistOp>) {
    while let Some(op) = rx.recv().await {
        match op {
            PersistOp::AppendEvent(event) => {
                if let Err(e) = storage.append_event(&event).await {
                    tracing::warn!(error = %e, "failed to persist event");
                }
            }
            PersistOp::SaveMeta(meta) => {
                if let Err(e) = storage.save_node_meta(&meta).await {
                    tracing::warn!(error = %e, "failed to persist node meta");
                }
            }
            PersistOp::SavePeers(pf) => {
                if let Err(e) = storage.save_peers(&pf).await {
                    tracing::warn!(error = %e, "failed to persist peers");
                }
            }
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
