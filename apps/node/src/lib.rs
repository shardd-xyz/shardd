// Re-export modules needed by integration tests.
pub mod app_state;
pub mod batch_writer;
pub mod bucket_allocator;
pub mod orphan_detector;
pub mod server;
pub mod service;
pub mod state;

pub use server::{NodeConfig, NodeHandle};

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

/// Node lifecycle phase for readiness gate and graceful shutdown.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodePhase {
    Warming = 0,
    Ready = 1,
    ShuttingDown = 2,
}

impl NodePhase {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Ready,
            2 => Self::ShuttingDown,
            _ => Self::Warming,
        }
    }
}

pub type PhaseRef = Arc<AtomicU8>;

pub fn get_phase(phase: &PhaseRef) -> NodePhase {
    NodePhase::from_u8(phase.load(Ordering::Relaxed))
}

pub fn set_phase(phase: &PhaseRef, p: NodePhase) {
    phase.store(p as u8, Ordering::Relaxed);
}
