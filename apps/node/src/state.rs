use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use shardd_storage::Storage;
use shardd_types::{Event, NodeMeta, PeersFile};

use crate::peer::PeerSet;

pub type SharedState = Arc<Mutex<NodeState>>;

/// All in-memory state for a node.
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
    /// Recompute contiguous head for a single origin.
    fn recompute_head(&mut self, origin: &str) {
        let current = self.contiguous_heads.get(origin).copied().unwrap_or(0);
        if let Some(seqs) = self.events_by_origin.get(origin) {
            let mut head = current;
            while seqs.contains_key(&(head + 1)) {
                head += 1;
            }
            self.contiguous_heads.insert(origin.to_string(), head);
        }
    }

    /// Insert an event idempotently. Returns true if new.
    pub async fn insert_event(&mut self, event: Event) -> anyhow::Result<bool> {
        let origin = event.origin_node_id.clone();
        let seq = event.origin_seq;
        let origin_map = self.events_by_origin.entry(origin.clone()).or_default();
        if origin_map.contains_key(&seq) {
            return Ok(false);
        }
        self.storage.append_event(&event).await?;
        origin_map.insert(seq, event);
        self.recompute_head(&origin);
        Ok(true)
    }

    /// Create a new local event, persist, and return it.
    pub async fn create_local_event(
        &mut self,
        amount: i64,
        note: Option<String>,
    ) -> anyhow::Result<Event> {
        let event = Event {
            event_id: uuid::Uuid::new_v4().to_string(),
            origin_node_id: self.node_id.clone(),
            origin_seq: self.next_seq,
            created_at_unix_ms: now_ms(),
            amount,
            note,
        };
        self.next_seq += 1;

        // Persist next_seq
        let meta = NodeMeta {
            node_id: self.node_id.clone(),
            host: self.addr.split(':').next().unwrap_or("127.0.0.1").to_string(),
            port: self
                .addr
                .split(':')
                .nth(1)
                .and_then(|p| p.parse().ok())
                .unwrap_or(0),
            next_seq: self.next_seq,
        };
        self.storage.save_node_meta(&meta).await?;

        // Insert into memory + disk
        let origin = event.origin_node_id.clone();
        let seq = event.origin_seq;
        self.storage.append_event(&event).await?;
        self.events_by_origin
            .entry(origin.clone())
            .or_default()
            .insert(seq, event.clone());
        self.recompute_head(&origin);

        Ok(event)
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

    /// Deterministic checksum: iterate events ordered by (origin asc, seq asc), hash.
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

    /// Get events in [from_seq, to_seq] for an origin.
    pub fn get_events_range(&self, origin: &str, from_seq: u64, to_seq: u64) -> Vec<Event> {
        let Some(seqs) = self.events_by_origin.get(origin) else {
            return vec![];
        };
        seqs.range(from_seq..=to_seq)
            .map(|(_, e)| e.clone())
            .collect()
    }

    /// All events sorted for presentation.
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

    /// Save current peer set to disk.
    pub async fn persist_peers(&self) -> anyhow::Result<()> {
        let pf = PeersFile {
            peers: self.peers.to_vec(),
        };
        self.storage.save_peers(&pf).await?;
        Ok(())
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
