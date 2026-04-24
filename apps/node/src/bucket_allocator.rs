//! Per-`(bucket, node_id)` seq + epoch allocator.
//!
//! Each bucket this node writes to owns its own independent epoch/seq line
//! (`OriginKey = (bucket, origin_node_id, origin_epoch, origin_seq)`). On
//! node startup every existing allocator row is flagged `needs_bump = TRUE`;
//! the first write to a bucket after startup atomically bumps the epoch
//! and clears the flag (§13.1). Buckets we never write to never bump, so
//! empty restarts don't accumulate empty epochs — which is the problem
//! the old per-node `increment_epoch(self)` path ran into.
//!
//! Once the lazy bump has happened for a bucket, subsequent writes go
//! through an in-memory `AtomicU64` fetch_add and periodically checkpoint
//! the `next_seq` back to durable storage.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::Result;
use dashmap::DashMap;
use tokio::sync::Mutex;
use tracing::warn;

use shardd_storage::StorageBackend;

/// In-memory state for one bucket's seq/epoch counter on this node.
#[derive(Debug)]
pub struct BucketAllocator {
    pub current_epoch: AtomicU64, // u32 in DB; u64 here so we can store in AtomicU64 cheaply
    pub next_seq: AtomicU64,
    pub needs_bump: AtomicBool,
    /// Serializes the lazy-bump critical section so two concurrent writers
    /// to the same bucket-after-restart agree on a single new epoch.
    pub bump_lock: Mutex<()>,
}

impl BucketAllocator {
    fn new(current_epoch: u32, next_seq: u64, needs_bump: bool) -> Self {
        Self {
            current_epoch: AtomicU64::new(current_epoch as u64),
            next_seq: AtomicU64::new(next_seq),
            needs_bump: AtomicBool::new(needs_bump),
            bump_lock: Mutex::new(()),
        }
    }

    pub fn epoch(&self) -> u32 {
        self.current_epoch.load(Ordering::Relaxed) as u32
    }

    pub fn peek_next_seq(&self) -> u64 {
        self.next_seq.load(Ordering::Relaxed)
    }
}

/// Registry of allocators keyed by bucket name.
#[derive(Clone)]
pub struct BucketAllocators<S: StorageBackend> {
    node_id: Arc<str>,
    storage: Arc<S>,
    map: Arc<DashMap<String, Arc<BucketAllocator>>>,
}

impl<S: StorageBackend> BucketAllocators<S> {
    /// Build a fresh registry. Call `load_from_storage` right after.
    pub fn new(node_id: Arc<str>, storage: Arc<S>) -> Self {
        Self {
            node_id,
            storage,
            map: Arc::new(DashMap::new()),
        }
    }

    /// Startup flow per §13.1:
    /// 1. Flag every existing row `needs_bump = TRUE` atomically in the DB.
    /// 2. Load them into memory. Each in-memory entry inherits the flag.
    pub async fn load_from_storage(&self) -> Result<()> {
        self.storage
            .mark_bucket_allocators_pending(&self.node_id)
            .await?;
        let rows = self.storage.load_bucket_allocators(&self.node_id).await?;
        for row in rows {
            self.map.insert(
                row.bucket.clone(),
                Arc::new(BucketAllocator::new(
                    row.current_epoch,
                    row.next_seq,
                    row.needs_bump,
                )),
            );
        }
        Ok(())
    }

    /// Allocate a fresh `(epoch, seq)` for `bucket`. On first call after
    /// startup (or first-ever call for a new bucket), this goes through
    /// the DB to establish durable epoch. On subsequent calls it's a pure
    /// in-memory atomic increment.
    pub async fn allocate(&self, bucket: &str) -> Result<(u32, u64)> {
        let allocator = match self.map.get(bucket) {
            Some(a) => a.clone(),
            None => {
                // First time we've seen this bucket in this process.
                // `bump_bucket_epoch` will insert a fresh row if none
                // exists, or bump an existing row if `needs_bump` is set.
                let epoch = self
                    .storage
                    .bump_bucket_epoch(bucket, &self.node_id)
                    .await?;
                let row = self.storage.load_bucket_allocators(&self.node_id).await?;
                let (next_seq, needs_bump) = row
                    .iter()
                    .find(|r| r.bucket == bucket)
                    .map(|r| (r.next_seq, r.needs_bump))
                    .unwrap_or((1, false));
                let _ = epoch;
                let alloc = Arc::new(BucketAllocator::new(epoch, next_seq, needs_bump));
                self.map.insert(bucket.to_string(), alloc.clone());
                alloc
            }
        };

        // Lazy-bump path. `needs_bump` is set at startup for every bucket
        // that already had a row; cleared once we've durably bumped. The
        // `bump_lock` makes two concurrent writers agree on a single new
        // epoch, not race two bumps back-to-back.
        if allocator.needs_bump.load(Ordering::Acquire) {
            let _guard = allocator.bump_lock.lock().await;
            // Re-check inside the lock.
            if allocator.needs_bump.load(Ordering::Acquire) {
                let new_epoch = self
                    .storage
                    .bump_bucket_epoch(bucket, &self.node_id)
                    .await?;
                allocator
                    .current_epoch
                    .store(new_epoch as u64, Ordering::Release);
                allocator.next_seq.store(1, Ordering::Release);
                allocator.needs_bump.store(false, Ordering::Release);
            }
        }

        let epoch = allocator.epoch();
        let seq = allocator.next_seq.fetch_add(1, Ordering::Relaxed);
        Ok((epoch, seq))
    }

    /// Best-effort checkpoint of `next_seq` back to durable storage. Call
    /// this after batch flushes so a crash can't lose many allocated but
    /// not-yet-persisted seqs. The DB side uses `GREATEST` so a stale
    /// checkpoint can't rewind the durable counter.
    pub async fn checkpoint_all(&self) {
        let node_id = self.node_id.clone();
        for entry in self.map.iter() {
            let bucket = entry.key().clone();
            let next_seq = entry.value().peek_next_seq();
            if let Err(error) = self
                .storage
                .persist_bucket_next_seq(&bucket, &node_id, next_seq)
                .await
            {
                warn!(bucket = %bucket, error = %error, "bucket allocator checkpoint failed");
            }
        }
    }

    /// Current in-memory epoch for `bucket`, or `None` if we haven't
    /// allocated from that bucket in this process yet.
    pub fn peek_epoch(&self, bucket: &str) -> Option<u32> {
        self.map.get(bucket).map(|a| a.epoch())
    }

    /// Drop the in-memory allocator entry for `bucket`. Used by the
    /// bucket-delete cascade (§3.5) — the durable allocator row is
    /// removed via `storage.delete_bucket_cascade`, but the in-memory
    /// counter has to be forgotten too so a future allocator race
    /// can't end up referencing stale state.
    pub fn forget(&self, bucket: &str) {
        self.map.remove(bucket);
    }
}
