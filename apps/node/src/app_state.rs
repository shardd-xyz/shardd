use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use shardd_broadcast::Broadcaster;
use shardd_storage::StorageBackend;

use crate::state::SharedState;

#[derive(Clone)]
pub struct AppState<S: StorageBackend> {
    pub shared: SharedState<S>,
    pub broadcaster: Arc<dyn Broadcaster>,
    pub metrics: Arc<RequestMetrics>,
}

#[derive(Default)]
pub struct RequestMetrics {
    inflight_requests: AtomicU64,
    completed_requests: AtomicU64,
    failed_requests: AtomicU64,
}

impl RequestMetrics {
    const OVERLOADED_INFLIGHT_THRESHOLD: u64 = 128;

    pub(crate) fn on_request_start(&self) {
        self.inflight_requests.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn on_request_finish(&self, failed: bool) {
        self.inflight_requests.fetch_sub(1, Ordering::Relaxed);
        self.completed_requests.fetch_add(1, Ordering::Relaxed);
        if failed {
            self.failed_requests.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn snapshot(&self) -> RequestMetricsSnapshot {
        let inflight_requests = self.inflight_requests.load(Ordering::Relaxed);
        RequestMetricsSnapshot {
            inflight_requests,
            completed_requests: self.completed_requests.load(Ordering::Relaxed),
            failed_requests: self.failed_requests.load(Ordering::Relaxed),
            overloaded: inflight_requests >= Self::OVERLOADED_INFLIGHT_THRESHOLD,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RequestMetricsSnapshot {
    pub(crate) inflight_requests: u64,
    pub(crate) completed_requests: u64,
    pub(crate) failed_requests: u64,
    pub(crate) overloaded: bool,
}
