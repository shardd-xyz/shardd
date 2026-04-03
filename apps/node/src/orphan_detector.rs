//! Orphan recovery per protocol.md §3.4.
//!
//! Scans for events in memory that haven't been confirmed as persisted.
//! Events older than the age threshold are written to the database.

use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

use shardd_storage::StorageBackend;
use shardd_types::Event;

/// Trait for accessing unpersisted events from SharedState without
/// knowing the concrete storage type parameter.
pub trait UnpersistedSource: Send + Sync {
    fn get_unpersisted_events(&self, cutoff_ms: u64) -> Vec<Event>;
    fn mark_persisted(&self, keys: &[(String, u32, u64)]);
}

pub struct OrphanDetector<S: StorageBackend> {
    source: Arc<dyn UnpersistedSource>,
    storage: Arc<S>,
    check_interval: Duration,
    age_threshold_ms: u64,
}

impl<S: StorageBackend> OrphanDetector<S> {
    pub fn new(
        source: Arc<dyn UnpersistedSource>,
        storage: Arc<S>,
        check_interval_ms: u64,
        age_threshold_ms: u64,
    ) -> Self {
        Self {
            source,
            storage,
            check_interval: Duration::from_millis(check_interval_ms),
            age_threshold_ms,
        }
    }

    pub async fn run(self) {
        let mut interval = tokio::time::interval(self.check_interval);
        interval.tick().await;

        loop {
            interval.tick().await;

            let now_ms = shardd_types::Event::now_ms();
            let cutoff = now_ms.saturating_sub(self.age_threshold_ms);
            let orphans = self.source.get_unpersisted_events(cutoff);

            if orphans.is_empty() {
                continue;
            }

            match self.storage.insert_events_bulk(&orphans).await {
                Ok(inserted) => {
                    if inserted > 0 {
                        info!(orphans = orphans.len(), inserted, "persisted orphaned events");
                    }
                    let keys: Vec<(String, u32, u64)> = orphans
                        .iter()
                        .map(|e| (e.origin_node_id.clone(), e.origin_epoch, e.origin_seq))
                        .collect();
                    self.source.mark_persisted(&keys);
                }
                Err(e) => {
                    warn!(error = %e, "orphan persistence failed, will retry");
                }
            }
        }
    }
}
