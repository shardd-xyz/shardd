//! Async batch persistence per protocol.md §3.3.
//!
//! Accumulates events from an mpsc channel and bulk-inserts to Postgres
//! every `flush_interval` or `flush_size` events. Not on the hot path.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use shardd_storage::StorageBackend;
use shardd_types::Event;

pub struct BatchWriter<S: StorageBackend> {
    rx: mpsc::UnboundedReceiver<Event>,
    storage: Arc<S>,
    flush_interval: Duration,
    flush_size: usize,
    matview_interval: Duration,
    /// Callback to notify state of persisted events.
    on_persisted: Option<Box<dyn Fn(&[(String, u32, u64)]) + Send + Sync>>,
}

impl<S: StorageBackend> BatchWriter<S> {
    pub fn new(
        rx: mpsc::UnboundedReceiver<Event>,
        storage: Arc<S>,
        flush_interval_ms: u64,
        flush_size: usize,
        matview_interval_ms: u64,
    ) -> Self {
        Self {
            rx,
            storage,
            flush_interval: Duration::from_millis(flush_interval_ms),
            flush_size,
            matview_interval: Duration::from_millis(matview_interval_ms),
            on_persisted: None,
        }
    }

    /// Set a callback invoked after each successful flush with the persisted keys.
    pub fn with_on_persisted(
        mut self,
        callback: impl Fn(&[(String, u32, u64)]) + Send + Sync + 'static,
    ) -> Self {
        self.on_persisted = Some(Box::new(callback));
        self
    }

    pub async fn run(mut self) {
        let mut buffer: Vec<Event> = Vec::new();
        let mut flush_timer = tokio::time::interval(self.flush_interval);
        let mut matview_timer = tokio::time::interval(self.matview_interval);

        flush_timer.tick().await;
        matview_timer.tick().await;

        loop {
            tokio::select! {
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
                            info!("BatchWriter: channel closed, shutting down");
                            return;
                        }
                    }
                }

                _ = flush_timer.tick() => {
                    if !buffer.is_empty() {
                        self.do_flush(&mut buffer).await;
                    }
                }

                _ = matview_timer.tick() => {
                    if let Err(e) = self.storage.refresh_balance_summary().await {
                        debug!(error = %e, "matview refresh failed");
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
                    debug!(total = count, inserted, "batch flushed");
                }
                if let Some(ref callback) = self.on_persisted {
                    let keys: Vec<(String, u32, u64)> = events
                        .iter()
                        .map(|e| (e.origin_node_id.clone(), e.origin_epoch, e.origin_seq))
                        .collect();
                    callback(&keys);
                }
            }
            Err(e) => {
                warn!(error = %e, count, "batch flush failed");
                // Events are lost from the buffer but remain in state.unpersisted.
                // OrphanDetector will pick them up.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shardd_storage::memory::InMemoryStorage;
    use shardd_types::EventType;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn make_event(seq: u64) -> Event {
        Event {
            event_id: format!("e{seq}"),
            origin_node_id: "n1".into(),
            origin_epoch: 1,
            origin_seq: seq,
            created_at_unix_ms: seq * 1000,
            r#type: EventType::Standard,
            bucket: "b".into(),
            account: "a".into(),
            amount: 1,
            note: None,
            idempotency_nonce: None,
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        }
    }

    #[tokio::test]
    async fn flushes_on_channel_close() {
        let storage = Arc::new(InMemoryStorage::new());
        let (tx, rx) = mpsc::unbounded_channel();

        let s = storage.clone();
        let handle = tokio::spawn(async move {
            BatchWriter::new(rx, s, 60_000, 1000, 60_000).run().await;
        });

        // Send 5 events then close channel
        for i in 1..=5 {
            tx.send(make_event(i)).unwrap();
        }
        drop(tx);

        handle.await.unwrap();

        // All 5 should be in storage
        assert_eq!(storage.event_count().await.unwrap(), 5);
    }

    #[tokio::test]
    async fn flushes_at_size_threshold() {
        let storage = Arc::new(InMemoryStorage::new());
        let (tx, rx) = mpsc::unbounded_channel();

        let s = storage.clone();
        let handle = tokio::spawn(async move {
            BatchWriter::new(rx, s, 60_000, 3, 60_000).run().await; // flush at 3
        });

        for i in 1..=5 {
            tx.send(make_event(i)).unwrap();
        }
        // Wait a bit for the size-triggered flush
        tokio::time::sleep(Duration::from_millis(50)).await;
        // At least the first 3 should be flushed
        assert!(storage.event_count().await.unwrap() >= 3);

        drop(tx);
        handle.await.unwrap();
        assert_eq!(storage.event_count().await.unwrap(), 5);
    }

    #[tokio::test]
    async fn calls_on_persisted_callback() {
        let storage = Arc::new(InMemoryStorage::new());
        let (tx, rx) = mpsc::unbounded_channel();
        let persisted_count = Arc::new(AtomicUsize::new(0));
        let pc = persisted_count.clone();

        let s = storage.clone();
        let handle = tokio::spawn(async move {
            BatchWriter::new(rx, s, 60_000, 1000, 60_000)
                .with_on_persisted(move |keys| {
                    pc.fetch_add(keys.len(), Ordering::Relaxed);
                })
                .run()
                .await;
        });

        for i in 1..=3 {
            tx.send(make_event(i)).unwrap();
        }
        drop(tx);
        handle.await.unwrap();

        assert_eq!(persisted_count.load(Ordering::Relaxed), 3);
    }
}
