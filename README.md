# shardd

Multi-writer replicated append-only event system demo.

Each node owns a local append-only sublog with gapless sequence numbers.
Replication is eventual — nodes sync by exchanging per-origin contiguous heads
and fetching missing suffix ranges over HTTP+JSON.

## Build

```
cargo build --release
```

## Run a 3-node cluster

Terminal 1 — Node A:
```
cargo run -- --port 3001 --config-dir ./demo/a
```

Terminal 2 — Node B (bootstrap from A):
```
cargo run -- --port 3002 --config-dir ./demo/b --bootstrap 127.0.0.1:3001
```

Terminal 3 — Node C (bootstrap from A):
```
cargo run -- --port 3003 --config-dir ./demo/c --bootstrap 127.0.0.1:3001
```

## Create events

```bash
curl -s -X POST localhost:3001/events \
  -H 'content-type: application/json' \
  -d '{"amount": 10, "note": "from A"}' | jq .

curl -s -X POST localhost:3002/events \
  -H 'content-type: application/json' \
  -d '{"amount": -3, "note": "from B"}' | jq .

curl -s -X POST localhost:3003/events \
  -H 'content-type: application/json' \
  -d '{"amount": 7, "note": "from C"}' | jq .
```

## Check convergence

Wait a few seconds for sync, then:

```bash
curl -s localhost:3001/state | jq '{event_count, balance, checksum}'
curl -s localhost:3002/state | jq '{event_count, balance, checksum}'
curl -s localhost:3003/state | jq '{event_count, balance, checksum}'
```

All three should show `event_count: 3`, `balance: 14`, and identical checksums.

## Other endpoints

```bash
# Health check
curl -s localhost:3001/health | jq .

# All events
curl -s localhost:3001/events | jq .

# Peer list
curl -s localhost:3001/peers | jq .

# Contiguous heads
curl -s localhost:3001/heads | jq .

# Add peer manually
curl -s -X POST localhost:3001/peers/add \
  -H 'content-type: application/json' \
  -d '{"addr": "127.0.0.1:3004"}'

# Trigger manual sync
curl -s -X POST localhost:3001/sync | jq .

# Debug a specific origin
curl -s localhost:3001/debug/origin/<node-id> | jq .

# Fetch event range for an origin
curl -s -X POST localhost:3001/events/range \
  -H 'content-type: application/json' \
  -d '{"origin_node_id": "<node-id>", "from_seq": 1, "to_seq": 5}'
```

## Restart scenario

1. Stop node C (Ctrl+C)
2. Create more events on A and B
3. Restart C with the same `--config-dir ./demo/c`
4. C reloads its persisted events and peers, then catches up via periodic sync

## CLI flags

| Flag | Default | Description |
|---|---|---|
| `--host` | `127.0.0.1` | Listen host |
| `--port` | (required) | Listen port |
| `--config-dir` | (required) | Data directory |
| `--bootstrap` | (none) | Bootstrap peer `host:port` |
| `--fanout` | `3` | Peers per sync round |
| `--sync-interval-ms` | `3000` | Sync interval |
| `--max-peers` | `16` | Max tracked peers |

## Storage layout

```
<config-dir>/
  node.json          # node_id, host, port, next_seq
  peers.json         # known peers
  events/
    <origin-id>.jsonl # one JSON event per line
```
# shardd
