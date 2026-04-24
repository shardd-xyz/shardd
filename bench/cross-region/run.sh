#!/bin/bash
# Cross-region bench runner for shardd (libp2p).
#
# Spins up a 10-node cluster across 5 simulated AWS regions with tc netem delays,
# then runs shardd-bench cross-region against it.
#
# Usage: ./run.sh [duration_secs] [concurrency_per_region] [converge_wait_secs]
#   CONVERGE_WAIT env var also honored.
set -e

cd "$(dirname "$0")"

# shardd-bench is also a mesh client: it reads SHARDD_CLUSTER_KEY from env
# (via clap). The docker-compose services get their key via the YAML
# anchor; export the same value here so the host-side `cargo run` of
# shardd-bench can join the private mesh.
export SHARDD_CLUSTER_KEY="${SHARDD_CLUSTER_KEY:-bench-cross-region-insecure-cluster-key}"

DURATION=${1:-30}
CONCURRENCY=${2:-50}
CONVERGE_WAIT=${3:-${CONVERGE_WAIT:-60}}

echo "=============================================="
echo "shardd cross-region benchmark (libp2p)"
echo "=============================================="
echo "  Regions:  us-east-1, us-west-2, eu-west-1, ap-southeast-1, sa-east-1"
echo "  Nodes:    10 (2 per region)"
echo "  Duration: ${DURATION}s"
echo "  Concurrency per region: ${CONCURRENCY}"
echo "  Convergence wait: ${CONVERGE_WAIT}s"
echo ""

echo "--- Building and starting cluster ---"
docker compose down -v 2>/dev/null || true
docker compose up -d --build

echo ""
echo "--- Waiting 15s for libp2p peer discovery ---"
sleep 15

echo ""
echo "--- Running cross-region benchmark ---"
cargo run --release -p shardd-bench -- cross-region \
  --bootstrap-peer /ip4/172.40.0.12/tcp/9000 \
  --duration-secs "$DURATION" \
  --concurrency "$CONCURRENCY" \
  --convergence-wait-secs "$CONVERGE_WAIT" \
  --expected-nodes 10

# ── Phase 2: restart-recovery + membership stability ─────────────────
#
# The initial load test catches throughput and convergence bugs, but
# nothing about mesh membership stability. Both libp2p bugs we hit in
# prod (num_established misaccounting → spurious MembershipEvent::Down
# storms; ephemeral ed25519 keypairs → stale Kademlia entries after
# restart) pass the load test cleanly because events still propagate.
#
# This phase catches that class of bug by:
#
#   1. Restarting one non-seed node mid-run
#   2. Letting the mesh reconverge
#   3. Running a short verification load to check the restarted node
#      rejoins discovery and writes still converge
#   4. Asserting the total count of "peer disconnected" INFO events is
#      bounded. Post-fix (num_established gating), only real peer-level
#      disconnects log at INFO — ~N per restart. Pre-fix, this number
#      balloons into the hundreds within seconds because every duplicate
#      connection prune logged a spurious disconnect.
RESTART_TARGET="us-west-2a"

echo ""
echo "--- Phase 2: restart recovery ($RESTART_TARGET) ---"
docker compose restart "$RESTART_TARGET"
echo "Restarted $RESTART_TARGET. Waiting 20s for mesh to reconverge..."
sleep 20

echo ""
echo "--- Verification load (10s, low concurrency) ---"
cargo run --release -p shardd-bench -- cross-region \
  --bootstrap-peer /ip4/172.40.0.12/tcp/9000 \
  --duration-secs 10 \
  --concurrency 10 \
  --convergence-wait-secs "$CONVERGE_WAIT" \
  --expected-nodes 10

# ── Membership stability assertion ────────────────────────────────────
#
# "peer disconnected" only fires at INFO when num_established → 0, i.e.
# a real peer-level terminal disconnect. Across this run the only
# expected sources are: the one restart in Phase 2 (~ up to 9 survivor
# peers each drop the connection to us-west-2a = 9 events) and a few
# stray transient disconnects during cold bootstrap. Anything north of
# ~40 means the num_established gating regressed and duplicate
# connections are being reported as peer-level disconnects again.
disconnect_count=$(docker compose logs 2>&1 | grep -c '"peer disconnected"' || true)
MAX_DISCONNECTS=${MAX_DISCONNECTS:-40}
echo ""
echo "--- Membership stability check ---"
echo "  \"peer disconnected\" INFO events in logs: $disconnect_count"
echo "  Threshold:                                  $MAX_DISCONNECTS"
if [ "$disconnect_count" -gt "$MAX_DISCONNECTS" ]; then
    echo "FAIL: membership churn ($disconnect_count) exceeds threshold ($MAX_DISCONNECTS)"
    echo ""
    echo "This likely indicates a regression in libp2p connection event handling"
    echo "(libs/broadcast/src/libp2p.rs: num_established gating on"
    echo " ConnectionEstablished/ConnectionClosed). Every duplicate connection"
    echo " prune is being counted as a real peer-level disconnect."
    echo ""
    echo "Sample of disconnect events (last 10):"
    docker compose logs 2>&1 | grep '"peer disconnected"' | tail -10
    docker compose down -v
    exit 1
fi
echo "✓ membership stability within threshold"

echo ""
echo "--- Tearing down cluster ---"
docker compose down -v

echo "=== DONE ==="
