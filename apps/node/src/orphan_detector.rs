//! Recovers events from crashed nodes.
//! Scans for events in memory that haven't been persisted to Postgres.

use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

use shardd_broadcast::Broadcaster;
use shardd_storage::postgres::PostgresStorage;
use shardd_types::Event;

use crate::state::SharedStateAny;

pub struct OrphanDetector {
    state: Arc<dyn SharedStateAny>,
    storage: Arc<PostgresStorage>,
    broadcaster: Arc<dyn Broadcaster>,
    check_interval: Duration,
    age_threshold_ms: u64,
}

impl OrphanDetector {
    pub fn new(
        state: Arc<dyn SharedStateAny>,
        storage: Arc<PostgresStorage>,
        broadcaster: Arc<dyn Broadcaster>,
        check_interval_ms: u64,
        age_threshold_ms: u64,
    ) -> Self {
        Self {
            state,
            storage,
            broadcaster,
            check_interval: Duration::from_millis(check_interval_ms),
            age_threshold_ms,
        }
    }

    pub async fn run(self) {
        let mut interval = tokio::time::interval(self.check_interval);
        interval.tick().await; // skip first immediate tick

        loop {
            interval.tick().await;

            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            let cutoff = now_ms.saturating_sub(self.age_threshold_ms);

            let orphans = self.state.get_unpersisted_events(cutoff);

            if orphans.is_empty() {
                continue;
            }

            match self.storage.insert_events_bulk(&orphans).await {
                Ok(inserted) => {
                    if inserted > 0 {
                        info!(orphans = orphans.len(), inserted, "persisted orphaned events");
                    }

                    let keys: Vec<(String, u64)> = orphans
                        .iter()
                        .map(|e| (e.origin_node_id.clone(), e.origin_seq))
                        .collect();
                    self.broadcaster.broadcast_persisted(&keys).await;
                }
                Err(e) => {
                    warn!(error = %e, "orphan persistence failed, will retry");
                }
            }
        }
    }
}
