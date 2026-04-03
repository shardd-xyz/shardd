//! Gossip-based broadcaster using foca (SWIM protocol) per §12.
//!
//! Provides automatic peer discovery, failure detection, and event
//! dissemination. Uses foca::AccumulatingRuntime for async integration.

use async_trait::async_trait;
use std::collections::BinaryHeap;
use std::cmp::Reverse;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use bytes::Bytes;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::{sleep_until, Instant};
use tracing::{debug, info, warn};

use shardd_types::Event;
use crate::{AckInfo, Broadcaster};

// ── foca Identity ────────────────────────────────────────────────────

#[derive(Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SwimId {
    pub addr: SocketAddr,
    pub node_id: String,
    pub epoch: u32,
}

impl std::fmt::Debug for SwimId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}({}:e{})", self.node_id, self.addr, self.epoch)
    }
}

impl foca::Identity for SwimId {
    type Addr = SocketAddr;
    fn addr(&self) -> SocketAddr { self.addr }
    fn renew(&self) -> Option<Self> {
        Some(Self { addr: self.addr, node_id: self.node_id.clone(), epoch: self.epoch + 1 })
    }
    fn win_addr_conflict(&self, other: &Self) -> bool { self.epoch > other.epoch }
}

// ── Broadcast handler for event dissemination ────────────────────────

/// Key for broadcast dedup. Events with same origin key invalidate older copies.
#[derive(Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
struct BcastKey {
    origin: String,
    epoch: u32,
    seq: u64,
}

impl foca::Invalidates for BcastKey {
    fn invalidates(&self, other: &Self) -> bool { self == other }
}

struct EventHandler {
    event_tx: mpsc::UnboundedSender<Event>,
}

impl foca::BroadcastHandler<SwimId> for EventHandler {
    type Key = BcastKey;
    type Error = anyhow::Error;

    fn receive_item(&mut self, data: &[u8], _sender: Option<&SwimId>) -> Result<Option<BcastKey>, anyhow::Error> {
        let event: Event = serde_json::from_slice(data)?;
        let key = BcastKey { origin: event.origin_node_id.clone(), epoch: event.origin_epoch, seq: event.origin_seq };
        let _ = self.event_tx.send(event);
        Ok(Some(key))
    }
}

// ── Input to the foca task ───────────────────────────────────────────

enum Input {
    Data(Bytes),
    Timer(foca::Timer<SwimId>),
    Announce(SwimId),
    Broadcast(Vec<u8>),
}

// ── GossipBroadcaster ────────────────────────────────────────────────

pub struct GossipConfig {
    pub bind_addr: SocketAddr,
    pub identity: SwimId,
    pub seeds: Vec<SocketAddr>,
    pub num_members_hint: u32,
}

pub struct GossipBroadcaster {
    input_tx: mpsc::Sender<Input>,
    member_count: Arc<AtomicUsize>,
    /// Receives events from remote peers.
    pub event_rx: mpsc::UnboundedReceiver<Event>,
}

impl GossipBroadcaster {
    pub async fn start(config: GossipConfig) -> anyhow::Result<Self> {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (input_tx, mut input_rx) = mpsc::channel::<Input>(1000);
        let member_count = Arc::new(AtomicUsize::new(0));

        let socket = Arc::new(UdpSocket::bind(config.bind_addr).await?);
        info!(addr = %config.bind_addr, "SWIM gossip socket bound");

        use rand_08::SeedableRng;
        let rng = rand_08::rngs::StdRng::from_entropy();
        let foca_config = foca::Config::new_wan(
            std::num::NonZeroU32::new(config.num_members_hint.max(2)).unwrap()
        );

        let handler = EventHandler { event_tx };
        let mut foca = foca::Foca::with_custom_broadcast(
            config.identity.clone(), foca_config, rng,
            foca::BincodeCodec(bincode::DefaultOptions::new()),
            handler,
        );

        // UDP send channel
        let (send_tx, mut send_rx) = mpsc::channel::<(SocketAddr, Vec<u8>)>(1000);
        let write_socket = socket.clone();
        tokio::spawn(async move {
            while let Some((dst, data)) = send_rx.recv().await {
                let _ = write_socket.send_to(&data, dst).await;
            }
        });

        // UDP recv task
        let recv_input_tx = input_tx.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            loop {
                match socket.recv_from(&mut buf).await {
                    Ok((len, _)) => {
                        let data = Bytes::copy_from_slice(&buf[..len]);
                        if recv_input_tx.send(Input::Data(data)).await.is_err() { break; }
                    }
                    Err(e) => warn!(error = %e, "UDP recv error"),
                }
            }
        });

        // Timer scheduler
        let timer_tx = input_tx.clone();
        let (sched_tx, mut sched_rx) = mpsc::unbounded_channel::<(Instant, foca::Timer<SwimId>)>();
        tokio::spawn(async move {
            let mut heap: BinaryHeap<Reverse<(Instant, foca::Timer<SwimId>)>> = BinaryHeap::new();
            loop {
                tokio::select! {
                    Some((when, timer)) = sched_rx.recv() => {
                        heap.push(Reverse((when, timer)));
                    }
                    _ = async {
                        if let Some(Reverse((when, _))) = heap.peek() {
                            sleep_until(*when).await;
                        } else {
                            std::future::pending::<()>().await;
                        }
                    } => {
                        if let Some(Reverse((_, timer))) = heap.pop() {
                            let _ = timer_tx.send(Input::Timer(timer)).await;
                        }
                    }
                }
            }
        });

        // Main foca task
        let mc = member_count.clone();
        let seeds: Vec<SwimId> = config.seeds.iter().map(|a| SwimId {
            addr: *a, node_id: String::new(), epoch: 0,
        }).collect();

        tokio::spawn(async move {
            // Custom runtime that collects outbound data
            struct Rt {
                to_send: Vec<(SwimId, Vec<u8>)>,
                to_schedule: Vec<(Duration, foca::Timer<SwimId>)>,
                notifications: Vec<foca::Notification<SwimId>>,
            }
            impl foca::Runtime<SwimId> for &mut Rt {
                fn notify(&mut self, n: foca::Notification<SwimId>) { self.notifications.push(n); }
                fn send_to(&mut self, to: SwimId, data: &[u8]) { self.to_send.push((to, data.to_vec())); }
                fn submit_after(&mut self, event: foca::Timer<SwimId>, after: Duration) { self.to_schedule.push((after, event)); }
            }

            let mut rt = Rt { to_send: vec![], to_schedule: vec![], notifications: vec![] };

            for seed in &seeds {
                let _ = foca.announce(seed.clone(), &mut rt);
            }
            drain(&mut rt, &send_tx, &sched_tx, &mc).await;

            while let Some(input) = input_rx.recv().await {
                match input {
                    Input::Timer(t) => { let _ = foca.handle_timer(t, &mut rt); }
                    Input::Data(d) => { let _ = foca.handle_data(&d, &mut rt); }
                    Input::Announce(dst) => { let _ = foca.announce(dst, &mut rt); }
                    Input::Broadcast(data) => { let _ = foca.add_broadcast(&data); }
                }
                drain(&mut rt, &send_tx, &sched_tx, &mc).await;
            }

            async fn drain(
                rt: &mut Rt,
                send_tx: &mpsc::Sender<(SocketAddr, Vec<u8>)>,
                sched_tx: &mpsc::UnboundedSender<(Instant, foca::Timer<SwimId>)>,
                mc: &Arc<AtomicUsize>,
            ) {
                for (to, data) in rt.to_send.drain(..) {
                    let _ = send_tx.send((foca::Identity::addr(&to), data)).await;
                }
                for (dur, timer) in rt.to_schedule.drain(..) {
                    let _ = sched_tx.send((Instant::now() + dur, timer));
                }
                for n in rt.notifications.drain(..) {
                    match n {
                        foca::Notification::MemberUp(id) => { mc.fetch_add(1, Ordering::Relaxed); info!(?id, "SWIM: member up"); }
                        foca::Notification::MemberDown(id) => { mc.fetch_sub(1, Ordering::Relaxed); info!(?id, "SWIM: member down"); }
                        _ => {}
                    }
                }
            }
        });

        Ok(Self { input_tx, member_count, event_rx })
    }
}

#[async_trait]
impl Broadcaster for GossipBroadcaster {
    async fn broadcast_event(&self, event: &Event, min_acks: u32, _ack_timeout_ms: u64) -> AckInfo {
        if let Ok(data) = serde_json::to_vec(event) {
            let _ = self.input_tx.send(Input::Broadcast(data)).await;
        }
        if min_acks > 0 {
            warn!(min_acks, "gossip does not support synchronous acks — use HTTP");
        }
        AckInfo::fire_and_forget()
    }

    async fn broadcast_persisted(&self, _keys: &[(String, u32, u64)]) {}

    async fn peer_count(&self) -> usize {
        self.member_count.load(Ordering::Relaxed)
    }
}
