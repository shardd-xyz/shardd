//! Async batch persistence — accumulates events and bulk-inserts to Postgres.
//! Not on the hot path: clients never wait for this.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use shardd_broadcast::Broadcaster;
use shardd_storage::postgres::PostgresStorage;
use shardd_types::Event;

pub struct BatchWriter {
    rx: mpsc::UnboundedReceiver<Event>,
    storage: Arc<PostgresStorage>,
    broadcaster: Arc<dyn Broadcaster>,
    flush_interval: Duration,
    flush_size: usize,
    matview_interval: Duration,
}

impl BatchWriter {
    pub fn new(
        rx: mpsc::UnboundedReceiver<Event>,
        storage: Arc<PostgresStorage>,
        broadcaster: Arc<dyn Broadcaster>,
        flush_interval_ms: u64,
        flush_size: usize,
        matview_interval_ms: u64,
    ) -> Self {
        Self {
            rx,
            storage,
            broadcaster,
            flush_interval: Duration::from_millis(flush_interval_ms),
            flush_size,
            matview_interval: Duration::from_millis(matview_interval_ms),
        }
    }

    pub async fn run(mut self) {
        let mut buffer: Vec<Event> = Vec::new();
        let mut flush_timer = tokio::time::interval(self.flush_interval);
        let mut matview_timer = tokio::time::interval(self.matview_interval);

        // Skip the first immediate tick
        flush_timer.tick().await;
        matview_timer.tick().await;

        loop {
            tokio::select! {
                // Receive events from the channel
                event = self.rx.recv() => {
                    match event {
                        Some(e) => {
                            buffer.push(e);
                            if buffer.len() >= self.flush_size {
                                self.do_flush(&mut buffer).await;
                            }
                        }
                        None => {
                            // Channel closed — flush remaining and exit
                            if !buffer.is_empty() {
                                self.do_flush(&mut buffer).await;
                            }
                            info!("BatchWriter shutting down");
                            return;
                        }
                    }
                }

                // Periodic flush timer
                _ = flush_timer.tick() => {
                    if !buffer.is_empty() {
                        self.do_flush(&mut buffer).await;
                    }
                }

                // Periodic materialized view refresh
                _ = matview_timer.tick() => {
                    if let Err(e) = self.storage.refresh_balance_summary().await {
                        debug!(error = %e, "matview refresh failed (may not exist yet)");
                    }
                }
            }
        }
    }

    async fn do_flush(&self, buffer: &mut Vec<Event>) {
        let events: Vec<Event> = buffer.drain(..).collect();
        let count = events.len();

        match self.storage.insert_events_bulk(&events).await {
            Ok(inserted) => {
                if inserted > 0 {
                    debug!(total = count, inserted, "batch flushed to Postgres");
                }

                // Broadcast persistence confirmations
                let keys: Vec<(String, u64)> = events
                    .iter()
                    .map(|e| (e.origin_node_id.clone(), e.origin_seq))
                    .collect();
                self.broadcaster.broadcast_persisted(&keys).await;
            }
            Err(e) => {
                warn!(error = %e, count, "batch flush failed — events will be retried by OrphanDetector");
                // Don't put them back in buffer — OrphanDetector will pick them up
                // from the unpersisted tracking in State
            }
        }
    }
}
