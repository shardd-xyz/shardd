#!/bin/bash
# Apply tc netem delay to peer-to-peer traffic only, then start shardd-node.
#
# The container's default iface (eth0) carries BOTH libp2p peer traffic and
# traffic back to host-side mesh clients. We only want to delay peer traffic.
# Strategy:
#
#   1. Install a `prio` qdisc with 3 bands (1:1 highest, 1:3 lowest).
#   2. Attach netem delay to band 1:3 — packets landing here are delayed.
#   3. Use a filter to send traffic destined for the docker bridge gateway
#      (where host-return packets go) to band 1:1 — no delay.
#   4. All other egress (peer-to-peer inside the 172.40.0.0/24 subnet)
#      falls through to the default band 1:3 and gets delayed.
#
# Environment variables:
#   DELAY_MS       — one-way delay in milliseconds (default 0)
#   JITTER_MS      — jitter in milliseconds (default 2)
#   REGION         — region label for logging
#   LIBP2P_PORT    — libp2p TCP port (default 9000)
#   DATABASE_URL   — Postgres connection string
#   BOOTSTRAP      — libp2p bootstrap multiaddr(s)
#   ADVERTISE_ADDR — externally reachable libp2p multiaddr to advertise
set -e

DELAY_MS=${DELAY_MS:-0}
JITTER_MS=${JITTER_MS:-2}
REGION=${REGION:-unknown}

if [ "$DELAY_MS" -gt 0 ]; then
    IFACE=$(ip route | awk '/default/ {print $5; exit}')
    GATEWAY=$(ip route | awk '/default/ {print $3; exit}')
    if [ -z "$IFACE" ] || [ -z "$GATEWAY" ]; then
        echo "WARNING: could not detect iface/gateway, skipping tc netem"
    else
        echo "tc: applying ${DELAY_MS}ms ± ${JITTER_MS}ms delay on $IFACE (region=$REGION, exempt gateway=$GATEWAY)"

        # prio qdisc: 3 bands, band 1 is highest priority (flowid 1:1), band 3 default (flowid 1:3).
        tc qdisc add dev "$IFACE" root handle 1: prio

        # Netem on band 1:3 — this delays everything that lands here.
        tc qdisc add dev "$IFACE" parent 1:3 handle 30: \
            netem delay "${DELAY_MS}ms" "${JITTER_MS}ms" distribution normal

        # Filter: traffic to the docker bridge gateway goes to band 1:1 (no delay).
        # This covers the HTTP response path back to the bench client on the host.
        tc filter add dev "$IFACE" protocol ip parent 1:0 prio 1 \
            u32 match ip dst "${GATEWAY}/32" flowid 1:1

        # Everything else (peer-to-peer traffic inside the docker subnet)
        # falls through to the default band 1:3 and gets the netem delay.
    fi
fi

ARGS=(
    --host 0.0.0.0
    --database-url "$DATABASE_URL"
    --libp2p-port "${LIBP2P_PORT:-9000}"
)

if [ -n "$BOOTSTRAP" ]; then
    for peer in $BOOTSTRAP; do
        ARGS+=(--bootstrap "$peer")
    done
fi

if [ -n "$ADVERTISE_ADDR" ]; then
    ARGS+=(--advertise-addr "$ADVERTISE_ADDR")
fi

echo "Starting shardd-node region=$REGION libp2p=${LIBP2P_PORT:-9000}"
exec shardd-node "${ARGS[@]}"
