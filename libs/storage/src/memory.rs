//! In-memory storage backend for unit tests.
//! Full-fidelity implementation matching PostgresStorage semantics:
//! dedup, conflict detection, epoch-aware queries, CRDT registry merge.

use anyhow::{bail, Result};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Mutex;

use shardd_types::{Event, EventType, NodeMeta, NodeRegistryEntry, NodeStatus};

use crate::{InsertResult, StorageBackend};

#[derive(Debug, Default)]
pub struct InMemoryStorage {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// (origin_node_id, origin_epoch, origin_seq) → Event
    events: BTreeMap<(String, u32, u64), Event>,
    /// event_id → (origin_node_id, origin_epoch, origin_seq)
    event_ids: HashMap<String, (String, u32, u64)>,
    node_meta: HashMap<String, NodeMeta>,
    registry: BTreeMap<String, NodeRegistryEntry>,
}

impl InMemoryStorage {
    pub fn new() -> Self {
        Self::default()
    }
}

impl StorageBackend for InMemoryStorage {
    async fn insert_event(&self, event: &Event) -> Result<InsertResult> {
        let mut inner = self.inner.lock().unwrap();
        let key = (event.origin_node_id.clone(), event.origin_epoch, event.origin_seq);

        // Check PK collision
        if let Some(existing_key) = inner.event_ids.get(&event.event_id) {
            if *existing_key != key {
                return Ok(InsertResult::Conflict {
                    details: format!("event_id {} PK collision", event.event_id),
                });
            }
        }

        // Check dedup key collision
        if let Some(existing) = inner.events.get(&key) {
            if existing.event_id == event.event_id {
                return Ok(InsertResult::Duplicate);
            } else {
                return Ok(InsertResult::Conflict {
                    details: format!(
                        "({}, {}, {}) has event_id {} but received {}",
                        key.0, key.1, key.2, existing.event_id, event.event_id
                    ),
                });
            }
        }

        inner.events.insert(key.clone(), event.clone());
        inner.event_ids.insert(event.event_id.clone(), key);
        Ok(InsertResult::Inserted)
    }

    async fn insert_events_bulk(&self, events: &[Event]) -> Result<usize> {
        let mut count = 0;
        for event in events {
            if let InsertResult::Inserted = self.insert_event(event).await? {
                count += 1;
            }
        }
        Ok(count)
    }

    async fn query_events_range(&self, origin: &str, epoch: u32, from_seq: u64, to_seq: u64) -> Result<Vec<Event>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner.events
            .range((origin.to_string(), epoch, from_seq)..=(origin.to_string(), epoch, to_seq))
            .map(|(_, e)| e.clone())
            .collect())
    }

    async fn query_all_events_sorted(&self) -> Result<Vec<Event>> {
        let inner = self.inner.lock().unwrap();
        let mut events: Vec<Event> = inner.events.values().cloned().collect();
        events.sort_by(|a, b| {
            a.created_at_unix_ms.cmp(&b.created_at_unix_ms)
                .then_with(|| a.origin_node_id.cmp(&b.origin_node_id))
                .then_with(|| a.origin_epoch.cmp(&b.origin_epoch))
                .then_with(|| a.origin_seq.cmp(&b.origin_seq))
        });
        Ok(events)
    }

    async fn event_count(&self) -> Result<usize> {
        Ok(self.inner.lock().unwrap().events.len())
    }

    async fn aggregate_balances(&self) -> Result<Vec<(String, String, i64)>> {
        let inner = self.inner.lock().unwrap();
        let mut balances: BTreeMap<(String, String), i64> = BTreeMap::new();
        for event in inner.events.values() {
            *balances.entry((event.bucket.clone(), event.account.clone())).or_default() += event.amount;
        }
        Ok(balances.into_iter().map(|((b, a), sum)| (b, a, sum)).collect())
    }

    async fn sequences_by_origin_epoch(&self) -> Result<BTreeMap<(String, u32), Vec<u64>>> {
        let inner = self.inner.lock().unwrap();
        let mut map: BTreeMap<(String, u32), Vec<u64>> = BTreeMap::new();
        for ((origin, epoch, seq), _) in &inner.events {
            map.entry((origin.clone(), *epoch)).or_default().push(*seq);
        }
        Ok(map)
    }

    async fn origin_account_epoch_mapping(&self) -> Result<Vec<(String, u32, String, String)>> {
        let inner = self.inner.lock().unwrap();
        let mut seen: HashSet<(String, u32, String, String)> = HashSet::new();
        for event in inner.events.values() {
            seen.insert((event.origin_node_id.clone(), event.origin_epoch, event.bucket.clone(), event.account.clone()));
        }
        Ok(seen.into_iter().collect())
    }

    async fn find_by_idempotency_key(&self, nonce: &str, bucket: &str, account: &str, amount: i64) -> Result<Vec<Event>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner.events.values()
            .filter(|e| {
                e.idempotency_nonce.as_deref() == Some(nonce)
                    && e.bucket == bucket
                    && e.account == account
                    && e.amount == amount
            })
            .cloned()
            .collect())
    }

    async fn active_holds(&self, now_ms: u64) -> Result<Vec<Event>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner.events.values()
            .filter(|e| {
                e.r#type == EventType::Standard
                    && e.amount < 0
                    && e.hold_amount > 0
                    && e.hold_expires_at_unix_ms > now_ms
            })
            .cloned()
            .collect())
    }

    async fn released_hold_refs(&self) -> Result<Vec<String>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner.events.values()
            .filter(|e| e.r#type == EventType::HoldRelease && e.void_ref.is_some())
            .filter_map(|e| e.void_ref.clone())
            .collect())
    }

    async fn checksum_data(&self) -> Result<String> {
        let inner = self.inner.lock().unwrap();
        let mut hasher = Sha256::new();
        let mut first = true;
        for ((_, _, _), event) in &inner.events {
            if !first { hasher.update(b"\n"); }
            first = false;
            hasher.update(event.canonical().as_bytes());
        }
        Ok(format!("{:x}", hasher.finalize()))
    }

    async fn load_node_meta(&self, node_id: &str) -> Result<Option<NodeMeta>> {
        Ok(self.inner.lock().unwrap().node_meta.get(node_id).cloned())
    }

    async fn save_node_meta(&self, meta: &NodeMeta) -> Result<()> {
        self.inner.lock().unwrap().node_meta.insert(meta.node_id.clone(), meta.clone());
        Ok(())
    }

    async fn increment_epoch(&self, node_id: &str) -> Result<u32> {
        let mut inner = self.inner.lock().unwrap();
        let meta = inner.node_meta.get_mut(node_id);
        match meta {
            Some(m) => {
                m.current_epoch += 1;
                m.next_seq = 1;
                Ok(m.current_epoch)
            }
            None => bail!("node_meta not found for {node_id}"),
        }
    }

    async fn derive_next_seq(&self, node_id: &str, epoch: u32) -> Result<u64> {
        let inner = self.inner.lock().unwrap();
        let max = inner.events
            .range((node_id.to_string(), epoch, 0)..=(node_id.to_string(), epoch, u64::MAX))
            .next_back()
            .map(|((_, _, seq), _)| *seq)
            .unwrap_or(0);
        Ok(max + 1)
    }

    async fn upsert_registry_entry(&self, entry: &NodeRegistryEntry) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let existing = inner.registry.get(&entry.node_id);
        let merged = match existing {
            Some(e) => e.merge(entry),
            None => entry.clone(),
        };
        inner.registry.insert(entry.node_id.clone(), merged);
        Ok(())
    }

    async fn load_registry(&self) -> Result<Vec<NodeRegistryEntry>> {
        Ok(self.inner.lock().unwrap().registry.values().cloned().collect())
    }

    async fn decommission_node(&self, node_id: &str) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(entry) = inner.registry.get_mut(node_id) {
            entry.status = NodeStatus::Decommissioned;
        }
        Ok(())
    }

    async fn refresh_balance_summary(&self) -> Result<()> {
        Ok(()) // no-op for in-memory
    }

    async fn read_balance_summary(&self) -> Result<Vec<(String, String, i64)>> {
        self.aggregate_balances().await
    }

    async fn run_migrations(&self) -> Result<()> {
        Ok(()) // no-op for in-memory
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(origin: &str, epoch: u32, seq: u64, amount: i64) -> Event {
        Event {
            event_id: format!("{origin}-{epoch}-{seq}"),
            origin_node_id: origin.into(),
            origin_epoch: epoch,
            origin_seq: seq,
            created_at_unix_ms: seq * 1000,
            r#type: EventType::Standard,
            bucket: "default".into(),
            account: "alice".into(),
            amount,
            note: None,
            idempotency_nonce: None,
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        }
    }

    #[tokio::test]
    async fn insert_and_dedup() {
        let s = InMemoryStorage::new();
        let e = make_event("n1", 1, 1, 100);
        assert_eq!(s.insert_event(&e).await.unwrap(), InsertResult::Inserted);
        assert_eq!(s.insert_event(&e).await.unwrap(), InsertResult::Duplicate);
    }

    #[tokio::test]
    async fn insert_conflict_different_event_id() {
        let s = InMemoryStorage::new();
        let e1 = make_event("n1", 1, 1, 100);
        s.insert_event(&e1).await.unwrap();

        let e2 = Event { event_id: "different".into(), ..e1.clone() };
        match s.insert_event(&e2).await.unwrap() {
            InsertResult::Conflict { .. } => {}
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn epoch_aware_range_query() {
        let s = InMemoryStorage::new();
        s.insert_event(&make_event("n1", 1, 1, 10)).await.unwrap();
        s.insert_event(&make_event("n1", 1, 2, 20)).await.unwrap();
        s.insert_event(&make_event("n1", 2, 1, 30)).await.unwrap(); // different epoch

        let range = s.query_events_range("n1", 1, 1, 2).await.unwrap();
        assert_eq!(range.len(), 2);
        assert_eq!(range[0].amount, 10);
        assert_eq!(range[1].amount, 20);

        // Different epoch returns separate events
        let range2 = s.query_events_range("n1", 2, 1, 1).await.unwrap();
        assert_eq!(range2.len(), 1);
        assert_eq!(range2[0].amount, 30);
    }

    #[tokio::test]
    async fn aggregate_balances_across_origins() {
        let s = InMemoryStorage::new();
        s.insert_event(&make_event("n1", 1, 1, 100)).await.unwrap();
        s.insert_event(&make_event("n2", 1, 1, -30)).await.unwrap();

        let balances = s.aggregate_balances().await.unwrap();
        assert_eq!(balances.len(), 1); // same bucket/account
        assert_eq!(balances[0].2, 70);
    }

    #[tokio::test]
    async fn sequences_by_origin_epoch_tracks_separately() {
        let s = InMemoryStorage::new();
        s.insert_event(&make_event("n1", 1, 1, 10)).await.unwrap();
        s.insert_event(&make_event("n1", 1, 2, 20)).await.unwrap();
        s.insert_event(&make_event("n1", 2, 1, 30)).await.unwrap();

        let seqs = s.sequences_by_origin_epoch().await.unwrap();
        assert_eq!(seqs[&("n1".into(), 1)], vec![1, 2]);
        assert_eq!(seqs[&("n1".into(), 2)], vec![1]);
    }

    #[tokio::test]
    async fn registry_crdt_merge_via_upsert() {
        let s = InMemoryStorage::new();

        let e1 = NodeRegistryEntry {
            node_id: "n1".into(), addr: "a:1".into(),
            first_seen_at_unix_ms: 100, last_seen_at_unix_ms: 500,
            status: NodeStatus::Active,
        };
        s.upsert_registry_entry(&e1).await.unwrap();

        let e2 = NodeRegistryEntry {
            node_id: "n1".into(), addr: "a:2".into(),
            first_seen_at_unix_ms: 200, last_seen_at_unix_ms: 600,
            status: NodeStatus::Decommissioned,
        };
        s.upsert_registry_entry(&e2).await.unwrap();

        let registry = s.load_registry().await.unwrap();
        assert_eq!(registry.len(), 1);
        assert_eq!(registry[0].status, NodeStatus::Decommissioned); // tombstone wins
        assert_eq!(registry[0].first_seen_at_unix_ms, 100); // MIN
        assert_eq!(registry[0].last_seen_at_unix_ms, 600); // MAX
    }

    #[tokio::test]
    async fn epoch_increment() {
        let s = InMemoryStorage::new();
        s.save_node_meta(&NodeMeta {
            node_id: "n1".into(), host: "h".into(), port: 0, current_epoch: 3, next_seq: 42,
        }).await.unwrap();

        let new_epoch = s.increment_epoch("n1").await.unwrap();
        assert_eq!(new_epoch, 4);

        let meta = s.load_node_meta("n1").await.unwrap().unwrap();
        assert_eq!(meta.current_epoch, 4);
        assert_eq!(meta.next_seq, 1); // reset on epoch increment
    }
}
