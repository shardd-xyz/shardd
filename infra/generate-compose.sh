#!/usr/bin/env bash
set -euo pipefail

# Generate compose.yml for N nodes
NODE_COUNT="${1:-64}"
BASE_PORT=3001
OUTPUT="${2:-$(dirname "$0")/compose.yml}"

cat > "$OUTPUT" <<'HEADER'
services:
HEADER

for i in $(seq 1 "$NODE_COUNT"); do
    port=$((BASE_PORT + i - 1))

    # Bootstrap from 2 neighbors (ring topology + node1 as anchor)
    prev=$(( ((i - 2 + NODE_COUNT) % NODE_COUNT) + 1 ))
    next=$(( (i % NODE_COUNT) + 1 ))
    prev_port=$((BASE_PORT + prev - 1))
    next_port=$((BASE_PORT + next - 1))

    # Always include node1 as bootstrap for fast convergence
    bootstraps="      - --bootstrap=127.0.0.1:${prev_port}"
    bootstraps+="\n      - --bootstrap=127.0.0.1:${next_port}"
    if [ "$prev" -ne 1 ] && [ "$next" -ne 1 ] && [ "$i" -ne 1 ]; then
        bootstraps+="\n      - --bootstrap=127.0.0.1:${BASE_PORT}"
    fi

    cat >> "$OUTPUT" <<EOF
  node${i}:
    image: shardd-node:latest
    container_name: shardd-node${i}
    network_mode: host
    command:
      - --host=0.0.0.0
      - --port=${port}
      - --advertise-addr=127.0.0.1:${port}
      - --config-dir=/data
$(echo -e "$bootstraps")
      - --max-peers=32
    volumes:
      - node${i}_data:/data
    restart: unless-stopped

EOF
done

# Dashboard
cat >> "$OUTPUT" <<'DASHBOARD'
  dashboard:
    image: shardd-dashboard:latest
    container_name: shardd-dashboard
    network_mode: host
    restart: unless-stopped

DASHBOARD

# Volumes
echo "volumes:" >> "$OUTPUT"
for i in $(seq 1 "$NODE_COUNT"); do
    echo "  node${i}_data:" >> "$OUTPUT"
done

echo "Generated $OUTPUT with $NODE_COUNT nodes (ports ${BASE_PORT}-$((BASE_PORT + NODE_COUNT - 1)))"
