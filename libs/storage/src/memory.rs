//! In-memory storage backend for unit tests.
//! Full-fidelity implementation matching PostgresStorage semantics:
//! dedup, conflict detection, epoch-aware queries, CRDT registry merge.

use anyhow::Result;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;

use shardd_types::{EpochKey, Event, EventType, NodeMeta, NodeRegistryEntry, NodeStatus};

use crate::{BucketAllocatorRow, EventsFilter, InsertResult, StorageBackend};

#[derive(Debug, Default)]
pub struct InMemoryStorage {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// (bucket, origin_node_id, origin_epoch, origin_seq) → Event
    events: BTreeMap<(String, String, u32, u64), Event>,
    /// event_id → (bucket, origin_node_id, origin_epoch, origin_seq)
    event_ids: HashMap<String, (String, String, u32, u64)>,
    node_meta: HashMap<String, NodeMeta>,
    registry: BTreeMap<String, NodeRegistryEntry>,
    /// Rolling prefix digests: (bucket, origin, epoch) → (head, digest)
    digests: BTreeMap<EpochKey, (u64, [u8; 32])>,
    /// (bucket, node_id) → BucketAllocatorRow
    bucket_allocators: BTreeMap<(String, String), BucketAllocatorRow>,
}

impl InMemoryStorage {
    pub fn new() -> Self {
        Self::default()
    }
}

impl StorageBackend for InMemoryStorage {
    async fn insert_event(&self, event: &Event) -> Result<InsertResult> {
        let mut inner = self.inner.lock().unwrap();
        let key = (
            event.bucket.clone(),
            event.origin_node_id.clone(),
            event.origin_epoch,
            event.origin_seq,
        );

        // Check PK collision (event_id → different dedup key)
        if let Some(existing_key) = inner.event_ids.get(&event.event_id)
            && *existing_key != key
        {
            return Ok(InsertResult::Conflict {
                details: format!("event_id {} PK collision", event.event_id),
            });
        }

        // Check dedup key collision
        if let Some(existing) = inner.events.get(&key) {
            if existing.event_id == event.event_id {
                return Ok(InsertResult::Duplicate);
            } else {
                return Ok(InsertResult::Conflict {
                    details: format!(
                        "({}, {}, {}, {}) has event_id {} but received {}",
                        key.0, key.1, key.2, key.3, existing.event_id, event.event_id
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

    async fn query_events_range(
        &self,
        bucket: &str,
        origin: &str,
        epoch: u32,
        from_seq: u64,
        to_seq: u64,
    ) -> Result<Vec<Event>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner
            .events
            .range(
                (bucket.to_string(), origin.to_string(), epoch, from_seq)
                    ..=(bucket.to_string(), origin.to_string(), epoch, to_seq),
            )
            .map(|(_, e)| e.clone())
            .collect())
    }

    async fn query_events_by_bucket(&self, bucket: &str) -> Result<Vec<Event>> {
        let inner = self.inner.lock().unwrap();
        let mut events: Vec<Event> = inner
            .events
            .iter()
            .filter(|((b, _, _, _), _)| b == bucket)
            .map(|(_, e)| e.clone())
            .collect();
        events.sort_by(|a, b| {
            a.origin_node_id
                .cmp(&b.origin_node_id)
                .then_with(|| a.origin_epoch.cmp(&b.origin_epoch))
                .then_with(|| a.origin_seq.cmp(&b.origin_seq))
        });
        Ok(events)
    }

    async fn delete_bucket_cascade(&self, bucket: &str) -> Result<()> {
        use shardd_types::META_BUCKET;
        if bucket == META_BUCKET {
            anyhow::bail!("refusing to delete the meta log itself");
        }
        let mut inner = self.inner.lock().unwrap();
        // Drop events for this bucket (and their event_ids).
        let doomed_keys: Vec<_> = inner
            .events
            .keys()
            .filter(|(b, _, _, _)| b == bucket)
            .cloned()
            .collect();
        for key in doomed_keys {
            if let Some(event) = inner.events.remove(&key) {
                inner.event_ids.remove(&event.event_id);
            }
        }
        // Drop digests and allocator rows.
        inner.digests.retain(|(b, _, _), _| b != bucket);
        inner.bucket_allocators.retain(|(b, _), _| b != bucket);
        Ok(())
    }

    async fn query_all_events_sorted(&self) -> Result<Vec<Event>> {
        let inner = self.inner.lock().unwrap();
        let mut events: Vec<Event> = inner.events.values().cloned().collect();
        events.sort_by(|a, b| {
            a.created_at_unix_ms
                .cmp(&b.created_at_unix_ms)
                .then_with(|| a.bucket.cmp(&b.bucket))
                .then_with(|| a.origin_node_id.cmp(&b.origin_node_id))
                .then_with(|| a.origin_epoch.cmp(&b.origin_epoch))
                .then_with(|| a.origin_seq.cmp(&b.origin_seq))
        });
        Ok(events)
    }

    async fn event_count(&self) -> Result<usize> {
        Ok(self.inner.lock().unwrap().events.len())
    }

    async fn query_events_filtered(
        &self,
        filter: &EventsFilter,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<Event>, u64)> {
        let inner = self.inner.lock().unwrap();
        let mut matches: Vec<&Event> = inner
            .events
            .values()
            .filter(|e| {
                if let Some(ref b) = filter.bucket
                    && e.bucket.as_str() != b.as_str()
                {
                    return false;
                }
                if let Some(ref p) = filter.bucket_prefix
                    && !e.bucket.starts_with(p.as_str())
                {
                    return false;
                }
                if let Some(ref a) = filter.account
                    && e.account.as_str() != a.as_str()
                {
                    return false;
                }
                if let Some(ref o) = filter.origin
                    && e.origin_node_id.as_str() != o.as_str()
                {
                    return false;
                }
                if let Some(ref t) = filter.event_type
                    && e.r#type.to_string().as_str() != t.as_str()
                {
                    return false;
                }
                if let Some(since) = filter.since_unix_ms
                    && e.created_at_unix_ms < since
                {
                    return false;
                }
                if let Some(until) = filter.until_unix_ms
                    && e.created_at_unix_ms > until
                {
                    return false;
                }
                if let Some(ref s) = filter.search {
                    let needle = s.to_ascii_lowercase();
                    let note_hit = e
                        .note
                        .as_deref()
                        .map(|n| n.to_ascii_lowercase().contains(&needle))
                        .unwrap_or(false);
                    let id_hit = e.event_id.to_ascii_lowercase().contains(&needle);
                    if !note_hit && !id_hit {
                        return false;
                    }
                }
                true
            })
            .collect();
        matches.sort_by(|a, b| {
            b.created_at_unix_ms
                .cmp(&a.created_at_unix_ms)
                .then_with(|| b.event_id.cmp(&a.event_id))
        });
        let total = matches.len() as u64;
        let page: Vec<Event> = matches
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .cloned()
            .collect();
        Ok((page, total))
    }

    async fn aggregate_balances(&self) -> Result<Vec<(String, String, i64)>> {
        let inner = self.inner.lock().unwrap();
        let mut balances: BTreeMap<(String, String), i64> = BTreeMap::new();
        for event in inner.events.values() {
            *balances
                .entry((event.bucket.clone(), event.account.clone()))
                .or_default() += event.amount;
        }
        Ok(balances
            .into_iter()
            .map(|((b, a), sum)| (b, a, sum))
            .collect())
    }

    async fn sequences_by_origin_epoch(&self) -> Result<BTreeMap<EpochKey, Vec<u64>>> {
        let inner = self.inner.lock().unwrap();
        let mut map: BTreeMap<EpochKey, Vec<u64>> = BTreeMap::new();
        for (bucket, origin, epoch, seq) in inner.events.keys() {
            map.entry((bucket.clone(), origin.clone(), *epoch))
                .or_default()
                .push(*seq);
        }
        Ok(map)
    }

    async fn origin_account_epoch_mapping(&self) -> Result<Vec<(String, String, u32, String)>> {
        let inner = self.inner.lock().unwrap();
        let mut seen: HashSet<(String, String, u32, String)> = HashSet::new();
        for event in inner.events.values() {
            seen.insert((
                event.bucket.clone(),
                event.origin_node_id.clone(),
                event.origin_epoch,
                event.account.clone(),
            ));
        }
        Ok(seen.into_iter().collect())
    }

    async fn find_by_idempotency_key(
        &self,
        nonce: &str,
        bucket: &str,
        account: &str,
        amount: i64,
    ) -> Result<Vec<Event>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner
            .events
            .values()
            .filter(|e| {
                e.idempotency_nonce == nonce
                    && e.bucket == bucket
                    && e.account == account
                    && e.amount == amount
            })
            .cloned()
            .collect())
    }

    async fn active_holds(&self, now_ms: u64) -> Result<Vec<Event>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner
            .events
            .values()
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
        Ok(inner
            .events
            .values()
            .filter(|e| e.r#type == EventType::HoldRelease && e.void_ref.is_some())
            .filter_map(|e| e.void_ref.clone())
            .collect())
    }

    async fn checksum_data(&self) -> Result<String> {
        let inner = self.inner.lock().unwrap();
        let mut hasher = Sha256::new();
        let mut first = true;
        // BTreeMap iteration is already ordered by (bucket, origin, epoch, seq).
        for event in inner.events.values() {
            if !first {
                hasher.update(b"\n");
            }
            first = false;
            hasher.update(event.canonical().as_bytes());
        }
        Ok(format!("{:x}", hasher.finalize()))
    }

    async fn load_node_meta(&self, node_id: &str) -> Result<Option<NodeMeta>> {
        Ok(self.inner.lock().unwrap().node_meta.get(node_id).cloned())
    }

    async fn save_node_meta(&self, meta: &NodeMeta) -> Result<()> {
        self.inner
            .lock()
            .unwrap()
            .node_meta
            .insert(meta.node_id.clone(), meta.clone());
        Ok(())
    }

    async fn load_bucket_allocators(&self, node_id: &str) -> Result<Vec<BucketAllocatorRow>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner
            .bucket_allocators
            .values()
            .filter(|row| row.node_id == node_id)
            .cloned()
            .collect())
    }

    async fn mark_bucket_allocators_pending(&self, node_id: &str) -> Result<usize> {
        let mut inner = self.inner.lock().unwrap();
        let mut count = 0;
        for row in inner.bucket_allocators.values_mut() {
            if row.node_id == node_id {
                row.needs_bump = true;
                count += 1;
            }
        }
        Ok(count)
    }

    async fn bump_bucket_epoch(&self, bucket: &str, node_id: &str) -> Result<u32> {
        let mut inner = self.inner.lock().unwrap();
        let key = (bucket.to_string(), node_id.to_string());
        let epoch = match inner.bucket_allocators.get_mut(&key) {
            Some(row) if row.needs_bump => {
                row.current_epoch += 1;
                row.next_seq = 1;
                row.needs_bump = false;
                row.current_epoch
            }
            Some(row) => row.current_epoch,
            None => {
                inner.bucket_allocators.insert(
                    key,
                    BucketAllocatorRow {
                        bucket: bucket.to_string(),
                        node_id: node_id.to_string(),
                        current_epoch: 1,
                        next_seq: 1,
                        needs_bump: false,
                    },
                );
                1
            }
        };
        Ok(epoch)
    }

    async fn persist_bucket_next_seq(
        &self,
        bucket: &str,
        node_id: &str,
        next_seq: u64,
    ) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let key = (bucket.to_string(), node_id.to_string());
        if let Some(row) = inner.bucket_allocators.get_mut(&key)
            && next_seq > row.next_seq
        {
            row.next_seq = next_seq;
        }
        Ok(())
    }

    async fn derive_next_seq(&self, bucket: &str, node_id: &str, epoch: u32) -> Result<u64> {
        let inner = self.inner.lock().unwrap();
        let max = inner
            .events
            .range(
                (bucket.to_string(), node_id.to_string(), epoch, 0)
                    ..=(bucket.to_string(), node_id.to_string(), epoch, u64::MAX),
            )
            .next_back()
            .map(|((_, _, _, seq), _)| *seq)
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
        Ok(self
            .inner
            .lock()
            .unwrap()
            .registry
            .values()
            .cloned()
            .collect())
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

    async fn load_digests(&self) -> Result<BTreeMap<EpochKey, (u64, [u8; 32])>> {
        Ok(self.inner.lock().unwrap().digests.clone())
    }

    async fn save_digest(
        &self,
        bucket: &str,
        origin: &str,
        epoch: u32,
        head: u64,
        digest: &[u8; 32],
    ) -> Result<()> {
        self.inner.lock().unwrap().digests.insert(
            (bucket.to_string(), origin.to_string(), epoch),
            (head, *digest),
        );
        Ok(())
    }

    async fn run_migrations(&self) -> Result<()> {
        Ok(()) // no-op for in-memory
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(bucket: &str, origin: &str, epoch: u32, seq: u64, amount: i64) -> Event {
        Event {
            event_id: format!("{bucket}-{origin}-{epoch}-{seq}"),
            origin_node_id: origin.into(),
            origin_epoch: epoch,
            origin_seq: seq,
            created_at_unix_ms: seq * 1000,
            r#type: EventType::Standard,
            bucket: bucket.into(),
            account: "alice".into(),
            amount,
            note: None,
            idempotency_nonce: format!("{bucket}-{origin}-{epoch}-{seq}-nonce"),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        }
    }

    #[tokio::test]
    async fn insert_and_dedup() {
        let s = InMemoryStorage::new();
        let e = make_event("default", "n1", 1, 1, 100);
        assert_eq!(s.insert_event(&e).await.unwrap(), InsertResult::Inserted);
        assert_eq!(s.insert_event(&e).await.unwrap(), InsertResult::Duplicate);
    }

    #[tokio::test]
    async fn insert_conflict_different_event_id() {
        let s = InMemoryStorage::new();
        let e1 = make_event("default", "n1", 1, 1, 100);
        s.insert_event(&e1).await.unwrap();

        let e2 = Event {
            event_id: "different".into(),
            ..e1.clone()
        };
        match s.insert_event(&e2).await.unwrap() {
            InsertResult::Conflict { .. } => {}
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn same_seq_in_different_buckets_is_not_a_conflict() {
        let s = InMemoryStorage::new();
        let a = make_event("bucket-a", "n1", 1, 1, 10);
        let b = make_event("bucket-b", "n1", 1, 1, 20);
        assert_eq!(s.insert_event(&a).await.unwrap(), InsertResult::Inserted);
        assert_eq!(s.insert_event(&b).await.unwrap(), InsertResult::Inserted);
    }

    #[tokio::test]
    async fn epoch_aware_range_query() {
        let s = InMemoryStorage::new();
        s.insert_event(&make_event("default", "n1", 1, 1, 10))
            .await
            .unwrap();
        s.insert_event(&make_event("default", "n1", 1, 2, 20))
            .await
            .unwrap();
        s.insert_event(&make_event("default", "n1", 2, 1, 30))
            .await
            .unwrap();

        let range = s
            .query_events_range("default", "n1", 1, 1, 2)
            .await
            .unwrap();
        assert_eq!(range.len(), 2);
        assert_eq!(range[0].amount, 10);
        assert_eq!(range[1].amount, 20);

        let range2 = s
            .query_events_range("default", "n1", 2, 1, 1)
            .await
            .unwrap();
        assert_eq!(range2.len(), 1);
        assert_eq!(range2[0].amount, 30);
    }

    #[tokio::test]
    async fn aggregate_balances_across_origins() {
        let s = InMemoryStorage::new();
        s.insert_event(&make_event("default", "n1", 1, 1, 100))
            .await
            .unwrap();
        s.insert_event(&make_event("default", "n2", 1, 1, -30))
            .await
            .unwrap();

        let balances = s.aggregate_balances().await.unwrap();
        assert_eq!(balances.len(), 1);
        assert_eq!(balances[0].2, 70);
    }

    #[tokio::test]
    async fn sequences_by_origin_epoch_tracks_separately() {
        let s = InMemoryStorage::new();
        s.insert_event(&make_event("default", "n1", 1, 1, 10))
            .await
            .unwrap();
        s.insert_event(&make_event("default", "n1", 1, 2, 20))
            .await
            .unwrap();
        s.insert_event(&make_event("default", "n1", 2, 1, 30))
            .await
            .unwrap();
        s.insert_event(&make_event("other", "n1", 1, 1, 40))
            .await
            .unwrap();

        let seqs = s.sequences_by_origin_epoch().await.unwrap();
        assert_eq!(seqs[&("default".into(), "n1".into(), 1)], vec![1, 2]);
        assert_eq!(seqs[&("default".into(), "n1".into(), 2)], vec![1]);
        assert_eq!(seqs[&("other".into(), "n1".into(), 1)], vec![1]);
    }

    #[tokio::test]
    async fn registry_crdt_merge_via_upsert() {
        let s = InMemoryStorage::new();

        let e1 = NodeRegistryEntry {
            node_id: "n1".into(),
            addr: "a:1".into(),
            first_seen_at_unix_ms: 100,
            last_seen_at_unix_ms: 500,
            status: NodeStatus::Active,
        };
        s.upsert_registry_entry(&e1).await.unwrap();

        let e2 = NodeRegistryEntry {
            node_id: "n1".into(),
            addr: "a:2".into(),
            first_seen_at_unix_ms: 200,
            last_seen_at_unix_ms: 600,
            status: NodeStatus::Decommissioned,
        };
        s.upsert_registry_entry(&e2).await.unwrap();

        let registry = s.load_registry().await.unwrap();
        assert_eq!(registry.len(), 1);
        assert_eq!(registry[0].status, NodeStatus::Decommissioned);
        assert_eq!(registry[0].first_seen_at_unix_ms, 100);
        assert_eq!(registry[0].last_seen_at_unix_ms, 600);
    }

    #[tokio::test]
    async fn bucket_allocator_lazy_first_bump() {
        let s = InMemoryStorage::new();

        // First-ever write to bucket A: allocator row gets created at
        // epoch=1, seq=1, needs_bump=false.
        let epoch = s.bump_bucket_epoch("A", "n1").await.unwrap();
        assert_eq!(epoch, 1);
        let rows = s.load_bucket_allocators("n1").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].current_epoch, 1);
        assert!(!rows[0].needs_bump);

        // Subsequent write, no bump pending → same epoch.
        let epoch = s.bump_bucket_epoch("A", "n1").await.unwrap();
        assert_eq!(epoch, 1);

        // Simulate a restart: mark pending, then next write should bump.
        let bumped = s.mark_bucket_allocators_pending("n1").await.unwrap();
        assert_eq!(bumped, 1);
        let epoch = s.bump_bucket_epoch("A", "n1").await.unwrap();
        assert_eq!(epoch, 2);

        // A different bucket B: first write gets epoch 1 regardless.
        let epoch_b = s.bump_bucket_epoch("B", "n1").await.unwrap();
        assert_eq!(epoch_b, 1);
    }

    #[tokio::test]
    async fn delete_bucket_cascade_drops_all_rows_for_that_bucket() {
        let s = InMemoryStorage::new();
        // Populate buckets A and B.
        s.insert_event(&make_event("A", "n1", 1, 1, 10))
            .await
            .unwrap();
        s.insert_event(&make_event("A", "n1", 1, 2, 20))
            .await
            .unwrap();
        s.insert_event(&make_event("B", "n1", 1, 1, 30))
            .await
            .unwrap();
        s.save_digest("A", "n1", 1, 2, &[1u8; 32]).await.unwrap();
        s.save_digest("B", "n1", 1, 1, &[2u8; 32]).await.unwrap();
        s.bump_bucket_epoch("A", "n1").await.unwrap();
        s.bump_bucket_epoch("B", "n1").await.unwrap();

        s.delete_bucket_cascade("A").await.unwrap();

        // A is wiped; B untouched.
        assert!(s.query_events_by_bucket("A").await.unwrap().is_empty());
        assert_eq!(s.query_events_by_bucket("B").await.unwrap().len(), 1);

        let digests = s.load_digests().await.unwrap();
        assert!(!digests.contains_key(&("A".into(), "n1".into(), 1)));
        assert!(digests.contains_key(&("B".into(), "n1".into(), 1)));

        let allocators = s.load_bucket_allocators("n1").await.unwrap();
        assert!(allocators.iter().all(|a| a.bucket != "A"));
        assert!(allocators.iter().any(|a| a.bucket == "B"));
    }

    #[tokio::test]
    async fn delete_bucket_cascade_refuses_meta() {
        let s = InMemoryStorage::new();
        assert!(
            s.delete_bucket_cascade(shardd_types::META_BUCKET)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn query_events_filtered_paginates_and_matches_filters() {
        let s = InMemoryStorage::new();
        // Populate: 5 events in A, 3 in B, different origins + times.
        for seq in 1..=5u64 {
            let mut e = make_event("A", "n1", 1, seq, seq as i64);
            e.created_at_unix_ms = 1_000 + seq * 10;
            s.insert_event(&e).await.unwrap();
        }
        for seq in 1..=3u64 {
            let mut e = make_event("B", "n2", 1, seq, seq as i64 + 100);
            e.created_at_unix_ms = 2_000 + seq * 10;
            s.insert_event(&e).await.unwrap();
        }
        // One bucket_delete meta tombstone for A.
        let tombstone = Event {
            event_id: "tomb".into(),
            origin_node_id: "n1".into(),
            origin_epoch: 1,
            origin_seq: 1,
            created_at_unix_ms: 3_000,
            r#type: EventType::BucketDelete,
            bucket: shardd_types::META_BUCKET.into(),
            account: "A".into(),
            amount: 0,
            note: Some("cleanup".into()),
            idempotency_nonce: "delete:A".into(),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };
        s.insert_event(&tombstone).await.unwrap();

        // No filter: total is 9, newest first, page size 4.
        let f = EventsFilter::default();
        let (page, total) = s.query_events_filtered(&f, 4, 0).await.unwrap();
        assert_eq!(total, 9);
        assert_eq!(page.len(), 4);
        assert!(
            page.windows(2)
                .all(|w| w[0].created_at_unix_ms >= w[1].created_at_unix_ms)
        );

        // Offset past end returns no rows, total unchanged.
        let (page, total) = s.query_events_filtered(&f, 10, 20).await.unwrap();
        assert_eq!(total, 9);
        assert!(page.is_empty());

        // Bucket filter.
        let f = EventsFilter {
            bucket: Some("A".into()),
            ..Default::default()
        };
        let (_, total) = s.query_events_filtered(&f, 100, 0).await.unwrap();
        assert_eq!(total, 5);

        // Event type filter (bucket_delete).
        let f = EventsFilter {
            event_type: Some("bucket_delete".into()),
            ..Default::default()
        };
        let (page, total) = s.query_events_filtered(&f, 100, 0).await.unwrap();
        assert_eq!(total, 1);
        assert_eq!(page[0].event_id, "tomb");

        // Time-range filter that excludes B.
        let f = EventsFilter {
            until_unix_ms: Some(1_500),
            ..Default::default()
        };
        let (_, total) = s.query_events_filtered(&f, 100, 0).await.unwrap();
        assert_eq!(total, 5); // only A 1..5 have created_at <= 1500

        // Search on note.
        let f = EventsFilter {
            search: Some("cleanup".into()),
            ..Default::default()
        };
        let (page, total) = s.query_events_filtered(&f, 100, 0).await.unwrap();
        assert_eq!(total, 1);
        assert_eq!(page[0].event_id, "tomb");
    }

    #[tokio::test]
    async fn query_events_filtered_bucket_prefix_scopes_to_one_namespace() {
        let s = InMemoryStorage::new();
        // Two users' internal bucket namespaces sharing the `user_` family
        // but distinct per-user prefixes. Prefix filter must only return
        // rows whose bucket starts with the specified prefix.
        for seq in 1..=4u64 {
            s.insert_event(&make_event("user_u1__bucket_a", "n1", 1, seq, seq as i64))
                .await
                .unwrap();
        }
        for seq in 1..=2u64 {
            s.insert_event(&make_event("user_u1__bucket_b", "n1", 1, seq, seq as i64))
                .await
                .unwrap();
        }
        for seq in 1..=3u64 {
            s.insert_event(&make_event("user_u2__bucket_a", "n2", 1, seq, seq as i64))
                .await
                .unwrap();
        }

        let f = EventsFilter {
            bucket_prefix: Some("user_u1__bucket_".into()),
            ..Default::default()
        };
        let (page, total) = s.query_events_filtered(&f, 100, 0).await.unwrap();
        assert_eq!(total, 6);
        assert!(
            page.iter()
                .all(|e| e.bucket.starts_with("user_u1__bucket_"))
        );

        // Prefix + bucket (exact) combine as AND.
        let f = EventsFilter {
            bucket_prefix: Some("user_u1__bucket_".into()),
            bucket: Some("user_u1__bucket_b".into()),
            ..Default::default()
        };
        let (_, total) = s.query_events_filtered(&f, 100, 0).await.unwrap();
        assert_eq!(total, 2);
    }

    #[tokio::test]
    async fn query_events_by_bucket_isolates_one_bucket() {
        let s = InMemoryStorage::new();
        s.insert_event(&make_event("A", "n1", 1, 1, 10))
            .await
            .unwrap();
        s.insert_event(&make_event("A", "n2", 1, 1, 20))
            .await
            .unwrap();
        s.insert_event(&make_event("B", "n1", 1, 1, 30))
            .await
            .unwrap();

        let a_events = s.query_events_by_bucket("A").await.unwrap();
        assert_eq!(a_events.len(), 2);
        assert!(a_events.iter().all(|e| e.bucket == "A"));
    }

    #[tokio::test]
    async fn rolling_digest_save_load_roundtrip() {
        let s = InMemoryStorage::new();
        let digest = [42u8; 32];
        s.save_digest("bucket-a", "origin-a", 1, 100, &digest)
            .await
            .unwrap();
        s.save_digest("bucket-b", "origin-b", 2, 50, &[7u8; 32])
            .await
            .unwrap();

        let loaded = s.load_digests().await.unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(
            loaded[&("bucket-a".into(), "origin-a".into(), 1)],
            (100, digest)
        );
        assert_eq!(
            loaded[&("bucket-b".into(), "origin-b".into(), 2)],
            (50, [7u8; 32])
        );

        let new_digest = [99u8; 32];
        s.save_digest("bucket-a", "origin-a", 1, 200, &new_digest)
            .await
            .unwrap();
        let loaded = s.load_digests().await.unwrap();
        assert_eq!(
            loaded[&("bucket-a".into(), "origin-a".into(), 1)],
            (200, new_digest)
        );
    }
}
