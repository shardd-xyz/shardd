use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

use shardd_storage::Storage;
use shardd_types::{Event, NodeMeta, PeersFile};

use crate::peer::PeerSet;

/// Shared state with granular locking. Read-heavy operations don't block each other.
#[derive(Clone)]
pub struct SharedState {
    pub inner: Arc<RwLock<NodeState>>,
    /// Write-behind channel for disk persistence (non-blocking).
    persist_tx: mpsc::UnboundedSender<PersistOp>,
}

enum PersistOp {
    AppendEvent(Event),
    SaveMeta(NodeMeta),
    SavePeers(PeersFile),
}

impl SharedState {
    pub fn new(state: NodeState) -> Self {
        let storage = state.storage.clone();
        let (tx, rx) = mpsc::unbounded_channel();
        let shared = Self {
            inner: Arc::new(RwLock::new(state)),
            persist_tx: tx,
        };
        tokio::spawn(persist_loop(storage, rx));
        shared
    }

    /// Insert an event idempotently. Returns true if new.
    /// Lock held only for in-memory insert; disk I/O is async via channel.
    pub async fn insert_event(&self, event: Event) -> anyhow::Result<bool> {
        let mut st = self.inner.write().await;
        let origin = event.origin_node_id.clone();
        let seq = event.origin_seq;
        let origin_map = st.events_by_origin.entry(origin.clone()).or_default();
        if origin_map.contains_key(&seq) {
            return Ok(false);
        }
        origin_map.insert(seq, event.clone());
        st.recompute_head(&origin);
        drop(st); // release lock before I/O
        let _ = self.persist_tx.send(PersistOp::AppendEvent(event));
        Ok(true)
    }

    /// Insert multiple events at once under a single lock acquisition.
    pub async fn insert_events_batch(&self, events: Vec<Event>) -> usize {
        let mut st = self.inner.write().await;
        let mut inserted = 0;
        let mut changed_origins = std::collections::HashSet::new();
        let mut to_persist = Vec::new();

        for event in events {
            let origin = event.origin_node_id.clone();
            let seq = event.origin_seq;
            let origin_map = st.events_by_origin.entry(origin.clone()).or_default();
            if !origin_map.contains_key(&seq) {
                origin_map.insert(seq, event.clone());
                changed_origins.insert(origin);
                to_persist.push(event);
                inserted += 1;
            }
        }

        for origin in changed_origins {
            st.recompute_head(&origin);
        }
        drop(st);

        for event in to_persist {
            let _ = self.persist_tx.send(PersistOp::AppendEvent(event));
        }
        inserted
    }

    /// Create a new local event. Lock held only for seq allocation + memory insert.
    pub async fn create_local_event(
        &self,
        amount: i64,
        note: Option<String>,
    ) -> anyhow::Result<Event> {
        let mut st = self.inner.write().await;
        let event = Event {
            event_id: uuid::Uuid::new_v4().to_string(),
            origin_node_id: st.node_id.clone(),
            origin_seq: st.next_seq,
            created_at_unix_ms: now_ms(),
            amount,
            note,
        };
        st.next_seq += 1;

        let meta = NodeMeta {
            node_id: st.node_id.clone(),
            host: st.addr.split(':').next().unwrap_or("127.0.0.1").to_string(),
            port: st
                .addr
                .split(':')
                .nth(1)
                .and_then(|p| p.parse().ok())
                .unwrap_or(0),
            next_seq: st.next_seq,
        };

        let origin = event.origin_node_id.clone();
        let seq = event.origin_seq;
        st.events_by_origin
            .entry(origin.clone())
            .or_default()
            .insert(seq, event.clone());
        st.recompute_head(&origin);
        drop(st); // release lock before I/O

        let _ = self.persist_tx.send(PersistOp::SaveMeta(meta));
        let _ = self.persist_tx.send(PersistOp::AppendEvent(event.clone()));
        Ok(event)
    }

    pub async fn persist_peers(&self) {
        let st = self.inner.read().await;
        let pf = PeersFile {
            peers: st.peers.to_vec(),
        };
        drop(st);
        let _ = self.persist_tx.send(PersistOp::SavePeers(pf));
    }
}

/// Background task that flushes writes to disk sequentially.
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

/// In-memory node state (no I/O methods).
pub struct NodeState {
    pub node_id: String,
    pub addr: String,
    pub next_seq: u64,
    pub peers: PeerSet,
    pub events_by_origin: BTreeMap<String, BTreeMap<u64, Event>>,
    pub contiguous_heads: BTreeMap<String, u64>,
    pub storage: Storage,
}

impl NodeState {
    pub fn recompute_head(&mut self, origin: &str) {
        let current = self.contiguous_heads.get(origin).copied().unwrap_or(0);
        if let Some(seqs) = self.events_by_origin.get(origin) {
            let mut head = current;
            while seqs.contains_key(&(head + 1)) {
                head += 1;
            }
            self.contiguous_heads.insert(origin.to_string(), head);
        }
    }

    pub fn event_count(&self) -> usize {
        self.events_by_origin.values().map(|m| m.len()).sum()
    }

    pub fn balance(&self) -> i64 {
        self.events_by_origin
            .values()
            .flat_map(|m| m.values())
            .map(|e| e.amount)
            .sum()
    }

    pub fn checksum(&self) -> String {
        let mut hasher = Sha256::new();
        for (origin, seqs) in &self.events_by_origin {
            for (seq, event) in seqs {
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

    pub fn get_events_range(&self, origin: &str, from_seq: u64, to_seq: u64) -> Vec<Event> {
        let Some(seqs) = self.events_by_origin.get(origin) else {
            return vec![];
        };
        seqs.range(from_seq..=to_seq)
            .map(|(_, e)| e.clone())
            .collect()
    }

    pub fn all_events_sorted(&self) -> Vec<Event> {
        let mut events: Vec<Event> = self
            .events_by_origin
            .values()
            .flat_map(|m| m.values().cloned())
            .collect();
        events.sort_by(|a, b| {
            a.created_at_unix_ms
                .cmp(&b.created_at_unix_ms)
                .then_with(|| a.origin_node_id.cmp(&b.origin_node_id))
                .then_with(|| a.origin_seq.cmp(&b.origin_seq))
        });
        events
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
