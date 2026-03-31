//! In-memory storage backend for testing.
//! Full-fidelity implementation matching Postgres semantics:
//! dedup, conflict detection, queries, peers.

use anyhow::{bail, Result};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Mutex;

use shardd_types::{Event, NodeMeta};

use crate::{InsertResult, StorageBackend};

#[derive(Debug, Default)]
pub struct InMemoryStorage {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// origin_node_id → (origin_seq → Event)
    events: BTreeMap<String, BTreeMap<u64, Event>>,
    /// event_id → (origin_node_id, origin_seq) for PK collision detection
    event_ids: BTreeMap<String, (String, u64)>,
    /// Node identity
    node_meta: BTreeMap<String, NodeMeta>,
    /// Known peers
    peers: BTreeSet<String>,
}

impl InMemoryStorage {
    pub fn new() -> Self {
        Self::default()
    }
}

impl StorageBackend for InMemoryStorage {
    async fn insert_event(&self, event: &Event) -> Result<InsertResult> {
        let mut inner = self.inner.lock().unwrap();

        // Check event_id PK collision
        if let Some((existing_origin, existing_seq)) = inner.event_ids.get(&event.event_id) {
            if existing_origin != &event.origin_node_id || *existing_seq != event.origin_seq {
                return Ok(InsertResult::Conflict {
                    details: format!(
                        "event_id {} already exists for ({}, {}) but received ({}, {})",
                        event.event_id, existing_origin, existing_seq,
                        event.origin_node_id, event.origin_seq
                    ),
                });
            }
        }

        // Check (origin, seq) unique constraint
        let origin_map = inner.events.entry(event.origin_node_id.clone()).or_default();
        if let Some(existing) = origin_map.get(&event.origin_seq) {
            if existing.event_id == event.event_id {
                return Ok(InsertResult::Duplicate);
            } else {
                return Ok(InsertResult::Conflict {
                    details: format!(
                        "({}, {}) has event_id {} but received {}",
                        event.origin_node_id, event.origin_seq,
                        existing.event_id, event.event_id
                    ),
                });
            }
        }

        // Insert
        origin_map.insert(event.origin_seq, event.clone());
        inner.event_ids.insert(
            event.event_id.clone(),
            (event.origin_node_id.clone(), event.origin_seq),
        );
        Ok(InsertResult::Inserted)
    }

    async fn save_node_meta(&self, meta: &NodeMeta) -> Result<()> {
        self.inner.lock().unwrap().node_meta.insert(meta.node_id.clone(), meta.clone());
        Ok(())
    }

    async fn save_peer(&self, addr: &str) -> Result<()> {
        self.inner.lock().unwrap().peers.insert(addr.to_string());
        Ok(())
    }

    async fn remove_peer(&self, addr: &str) -> Result<()> {
        self.inner.lock().unwrap().peers.remove(addr);
        Ok(())
    }

    async fn allocate_seq(&self, node_id: &str) -> Result<u64> {
        let mut inner = self.inner.lock().unwrap();
        let meta = inner.node_meta.get_mut(node_id);
        match meta {
            Some(m) => {
                let seq = m.next_seq;
                m.next_seq += 1;
                Ok(seq)
            }
            None => bail!("node_meta not found for {node_id}"),
        }
    }

    async fn query_events_range(&self, origin: &str, from_seq: u64, to_seq: u64) -> Result<Vec<Event>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner.events.get(origin)
            .map(|m| m.range(from_seq..=to_seq).map(|(_, e)| e.clone()).collect())
            .unwrap_or_default())
    }

    async fn query_all_events_sorted(&self) -> Result<Vec<Event>> {
        let inner = self.inner.lock().unwrap();
        let mut events: Vec<Event> = inner.events.values()
            .flat_map(|m| m.values().cloned())
            .collect();
        events.sort_by(|a, b| {
            a.created_at_unix_ms.cmp(&b.created_at_unix_ms)
                .then_with(|| a.origin_node_id.cmp(&b.origin_node_id))
                .then_with(|| a.origin_seq.cmp(&b.origin_seq))
        });
        Ok(events)
    }

    async fn aggregate_balances(&self) -> Result<Vec<(String, String, i64)>> {
        let inner = self.inner.lock().unwrap();
        let mut balances: BTreeMap<(String, String), i64> = BTreeMap::new();
        for events in inner.events.values() {
            for event in events.values() {
                *balances.entry((event.bucket.clone(), event.account.clone())).or_default() += event.amount;
            }
        }
        Ok(balances.into_iter().map(|((b, a), sum)| (b, a, sum)).collect())
    }

    async fn sequences_by_origin(&self) -> Result<BTreeMap<String, Vec<u64>>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner.events.iter()
            .map(|(origin, m)| (origin.clone(), m.keys().copied().collect()))
            .collect())
    }

    async fn sequences_from(&self, origin: &str, from_seq: u64) -> Result<Vec<u64>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner.events.get(origin)
            .map(|m| m.range(from_seq..).map(|(seq, _)| *seq).collect())
            .unwrap_or_default())
    }

    async fn event_count(&self) -> Result<usize> {
        let inner = self.inner.lock().unwrap();
        Ok(inner.events.values().map(|m| m.len()).sum())
    }

    async fn checksum_data(&self) -> Result<String> {
        let inner = self.inner.lock().unwrap();
        let mut hasher = Sha256::new();
        let mut first = true;
        // Canonical order: (origin ASC, seq ASC)
        for (origin, events) in &inner.events {
            for (seq, event) in events {
                if !first { hasher.update(b"\n"); }
                first = false;
                hasher.update(format!(
                    "{}:{}:{}:{}:{}:{}",
                    origin, seq, event.event_id,
                    event.bucket, event.account, event.amount,
                ));
            }
        }
        Ok(format!("{:x}", hasher.finalize()))
    }

    async fn origin_account_mapping(&self) -> Result<Vec<(String, String, String)>> {
        let inner = self.inner.lock().unwrap();
        let mut seen: HashSet<(String, String, String)> = HashSet::new();
        for (origin, events) in &inner.events {
            for event in events.values() {
                seen.insert((origin.clone(), event.bucket.clone(), event.account.clone()));
            }
        }
        Ok(seen.into_iter().collect())
    }

    async fn max_origin_seq(&self, origin: &str) -> Result<u64> {
        let inner = self.inner.lock().unwrap();
        Ok(inner.events.get(origin)
            .and_then(|m| m.keys().next_back().copied())
            .unwrap_or(0))
    }

    async fn load_node_meta_by_id(&self, node_id: &str) -> Result<Option<NodeMeta>> {
        Ok(self.inner.lock().unwrap().node_meta.get(node_id).cloned())
    }

    async fn derive_next_seq(&self, node_id: &str) -> Result<u64> {
        let inner = self.inner.lock().unwrap();
        let max = inner.events.get(node_id)
            .and_then(|m| m.keys().next_back().copied())
            .unwrap_or(0);
        Ok(max + 1)
    }

    async fn load_peers(&self) -> Result<Vec<String>> {
        Ok(self.inner.lock().unwrap().peers.iter().cloned().collect())
    }
}
