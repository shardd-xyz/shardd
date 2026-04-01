# Distributed Append-Only Ledger Protocol

Version 1.0

## 1. Overview

A distributed system where multiple independent nodes accept credit/debit events for named accounts. Each node maintains a full replica of all events. Nodes are eventually consistent — any node can accept writes independently, and events propagate to all other nodes asynchronously.

No consensus protocol. No leader election. No shared storage. Each node is fully independent and can operate in isolation.

## 2. Core Concepts

### 2.1 Event

The atomic unit of data. Immutable once created.

| Field | Type | Description |
|-------|------|-------------|
| `event_id` | string (UUID) | Globally unique identifier |
| `origin_node_id` | string | ID of the node that created this event |
| `origin_seq` | uint64 | Monotonically increasing sequence number, per origin node. Starts at 1, gapless. |
| `created_at_unix_ms` | uint64 | Creation timestamp (milliseconds since Unix epoch) |
| `bucket` | string | Top-level namespace (e.g., tenant, environment) |
| `account` | string | Account within the bucket |
| `amount` | int64 | Positive = credit, negative = debit |
| `note` | string (nullable) | Optional human-readable description |

The tuple `(origin_node_id, origin_seq)` is globally unique and serves as the deduplication key.

### 2.2 Node

An independent process with:
- A unique `node_id` (stable across restarts)
- Its own persistent storage (database)
- An HTTP API for clients and peer communication
- In-memory caches for fast reads

### 2.3 Balance

The sum of all `amount` values for a given `(bucket, account)` pair across all events from all origins.

### 2.4 Contiguous Head

For each origin, the highest sequence number N where all events 1 through N are present with no gaps. Used by the sync protocol to determine what events are missing.

### 2.5 Collapsed State

Per-account metadata indicating whether the balance is complete:
- **locally_confirmed**: every origin that has contributed events to this account has a contiguous head equal to its maximum known sequence. The balance is final given this node's current knowledge.
- **provisional**: at least one contributing origin has gaps (head < max known sequence). The balance may change when missing events arrive.

Note: "locally_confirmed" is scoped to this node's view. An origin this node hasn't heard from yet is not accounted for.

## 3. Event Lifecycle

### 3.1 Creation

A client sends a create request to any node. The receiving node:

1. **Validates the overdraft guard** (debits only): if the projected balance (`current_balance + amount`) would fall below the floor (`-max_overdraft` or 0), reject with an error. This check is atomic — no two concurrent debits can both pass if only one should.

2. **Assigns a sequence number**: the next value in this node's monotonic counter. Increment the counter.

3. **Generates a UUID** for the event_id.

4. **Updates in-memory caches**: balance, contiguous head, origin tracking. These updates are the source of truth for subsequent requests — not the database.

5. **Queues the event for persistence** to the node's own database (asynchronous, not blocking the client).

6. **Broadcasts the event to all known peers** (asynchronous). If the client requested quorum acknowledgment (`min_acks > 0`), waits until the specified number of peers confirm receipt or a timeout expires.

7. **Returns the event** to the client along with the updated balance and acknowledgment info.

### 3.2 Replication

When a node receives an event from a peer (via broadcast or sync):

1. **Deduplication check**: if `origin_seq <= contiguous_head` for this origin, or the event is already in the event buffer, discard as duplicate.

2. **Update in-memory caches**: balance, contiguous head (with gap tracking), origin tracking.

3. **Queue for persistence** to this node's own database.

The overdraft guard is NOT applied to replicated events. Replicated events are accepted unconditionally — the originating node already validated them.

### 3.3 Persistence

A background writer accumulates events and periodically writes them to the database in bulk:
- **Flush interval**: configurable (default 100ms)
- **Flush size**: configurable (default 1000 events)
- **Write method**: bulk insert with conflict-skip semantics (e.g., `INSERT ... ON CONFLICT DO NOTHING`)
- After a successful flush, notify peers that these events are now durable

### 3.4 Orphan Recovery

A background scanner periodically checks for events that are in memory but not yet confirmed as persisted to the database:
- **Scan interval**: configurable (default 500ms)
- **Age threshold**: configurable (default 500ms)
- Events older than the threshold that haven't been confirmed as persisted are written to the database
- This handles the case where the originating node crashes before its writer flushes

## 4. Sync Protocol

### 4.1 Broadcast (Primary)

The primary sync mechanism. When an event is created or received, it is immediately sent to all known peers via the broadcast layer.

Broadcast is fire-and-forget by default. With quorum acks, the broadcaster waits for a configurable number of peers to confirm receipt before returning to the caller.

### 4.2 Catch-up Sync (Safety Net)

A slow periodic sync runs as a safety net for events missed by broadcast (e.g., during network partitions or node downtime):

1. Get the list of known peers
2. For each peer, request their contiguous heads
3. Compare with local heads
4. For each origin where the peer is ahead, request the missing event range
5. Insert received events locally (dedup handles any overlap)

**Interval**: configurable (default 30 seconds). This is NOT the primary sync mechanism.

### 4.3 Trustless Bootstrap

When a new node joins or a node restarts with empty/partial storage:

1. Connect to the cluster via bootstrap peers
2. Request peer lists and heads from bootstrap peers
3. For ALL origins on ALL peers, fetch the complete event range from sequence 1 to the peer's head
4. Insert all events into local storage and in-memory caches
5. Recompute balances from events (`SUM(amount) GROUP BY bucket, account`)
6. Start serving

A new node NEVER trusts another node's balance values. It always recomputes from the full event log.

## 5. In-Memory State

Each node maintains these caches in memory for fast reads:

| Cache | Key | Value | Purpose |
|-------|-----|-------|---------|
| Balances | (bucket, account) | int64 (atomic) | Overdraft checks, API reads |
| Heads | origin_id | uint64 | Sync protocol, collapsed state |
| Account Origins | (bucket, account) | set of origin_ids | Collapsed state computation |
| Max Known Seqs | origin_id | uint64 | Collapsed state (detect gaps) |
| Event Buffer | (origin_id, seq) | Event | Orphan recovery, serve recent events |
| Unpersisted | (origin_id, seq) | timestamp | Track what's not yet in database |
| Pending Seqs | origin_id | sorted set of uint64 | In-memory head advancement |

### 5.1 Head Advancement

When an event arrives with sequence N for an origin:
- If N == current_head + 1: advance the head to N. Then check the pending set — if N+1, N+2, ... are present, advance through them and remove from the pending set.
- If N > current_head + 1: this is out-of-order. Add N to the pending set. Do not advance the head.
- If N <= current_head: duplicate, discard.

This is purely in-memory. No database queries on the hot path.

### 5.2 Balance Updates

Balances are updated atomically (compare-and-swap for debits, simple add for credits) at the time the event is accepted into the in-memory cache. The database is NOT consulted for balance reads or writes on the hot path.

On startup, balances are rebuilt from the database.

## 6. Persistence Layer

### 6.1 Database Schema

**events** — append-only event log:
- Primary key: `event_id`
- Unique constraint: `(origin_node_id, origin_seq)` — the dedup key
- Indexes: `(bucket, account)` for balance aggregation, `(created_at_unix_ms)` for time-ordered queries

**node_meta** — node identity:
- `node_id` (primary key), `host`, `port`, `next_seq`

**peers** — known peer addresses:
- `addr` (primary key)

**balance_summary** — materialized/cached view:
- `bucket`, `account`, `balance` (= SUM(amount) from events)
- Refreshed periodically (default every 5 seconds)
- Used for fast balance bootstrap on startup

### 6.2 Conflict Handling

All inserts use conflict-skip semantics on `(origin_node_id, origin_seq)`. If a row already exists with the same key, the insert is silently skipped.

For integrity verification: if a conflict is detected, the existing row's `event_id` should be compared with the incoming event's `event_id`. If they differ, this indicates data corruption or a sequence reuse bug — log a warning.

## 7. HTTP API

### 7.1 Client Endpoints

**POST /events** — Create a new event

Request:
```json
{
  "bucket": "string",
  "account": "string",
  "amount": 0,
  "note": "string | null",
  "max_overdraft": 0,
  "min_acks": 0,
  "ack_timeout_ms": 500
}
```

| Field | Default | Description |
|-------|---------|-------------|
| `max_overdraft` | 0 | How far below zero the balance can go. 0 = no overdraft. |
| `min_acks` | 0 | Minimum peer acknowledgments before responding. 0 = fire-and-forget. |
| `ack_timeout_ms` | 500 | Maximum time to wait for acks (ms). |

Response (201):
```json
{
  "event": { ... },
  "balance": 0,
  "acks": { "received": 0, "requested": 0, "timeout": false }
}
```

Response (422 — overdraft rejected):
```json
{
  "error": "insufficient_funds",
  "balance": 0,
  "projected_balance": 0,
  "limit": 0
}
```

**GET /health** — Node status
**GET /state** — Full state (heads, checksum, peers, event count, total balance)
**GET /events** — All events (sorted by timestamp, from database)
**GET /heads** — Contiguous head per origin
**GET /balances** — All account balances
**GET /collapsed** — Balance + sync completeness per account
**GET /collapsed/:bucket/:account** — Single account collapsed state
**GET /persistence** — Count and age of unpersisted events
**GET /debug/origin/:id** — Sequences and gaps for a specific origin

### 7.2 Peer Endpoints

**POST /events/replicate** — Accept a replicated event from a peer

Request: an Event object.
Response: `{"status": "ok", "inserted": true|false}`

**POST /events/range** — Fetch events for a specific origin in a sequence range

Request:
```json
{
  "origin_node_id": "string",
  "from_seq": 0,
  "to_seq": 0
}
```

Response: `[Event, Event, ...]`

**POST /join** — Cluster join handshake

Request: `{"node_id": "string", "addr": "string"}`
Response: `{"node_id": "string", "addr": "string", "peers": [...], "heads": {...}}`

**POST /peers/add** — Add a peer manually
**GET /peers** — List known peers

## 8. Checksum

For verifying convergence between nodes. Deterministic hash of all events.

### 8.1 Canonical Format

For each event, compute a string:
```
{origin_node_id}:{origin_seq}:{event_id}:{bucket}:{account}:{amount}
```

Order all events by `(origin_node_id ASC, origin_seq ASC)`.

Join with newline (`\n`).

Hash with SHA-256. Encode as lowercase hexadecimal (64 characters).

The `note` field is excluded (cosmetic, not financial state).

### 8.2 Comparison

Two nodes with the same checksum have identical event sets. If checksums differ, the nodes have divergent data — use the head-based sync protocol to find and resolve the difference.

## 9. Overdraft Guard

### 9.1 Behavior

The overdraft guard prevents a single node from allowing debits that would push an account below a configurable floor. It is checked atomically at event creation time.

- **Credits** (amount > 0): always succeed, no guard applied
- **Debits** (amount < 0): `current_balance + amount >= -max_overdraft` must hold
- **max_overdraft = 0**: balance cannot go below 0
- **max_overdraft = 500**: balance can go as low as -500

### 9.2 Limitations

The guard is LOCAL to the node receiving the request. It does not consult other nodes. In a multi-node scenario:
- Node A sees balance 1000, allows debit -800 → balance 200
- Node B also sees balance 1000 (hasn't synced A's debit yet), allows debit -800 → balance 200
- After sync: true balance = 1000 - 800 - 800 = -600 (overdraft!)

This is a known, documented limitation. The guard reduces the probability and magnitude of overdrafts but does not eliminate them in a distributed setting. Eliminating them would require consensus (e.g., Raft, Paxos), which this protocol explicitly avoids for performance and availability.

For use cases requiring strict overdraft prevention, route all debits for a given account to a single node.

## 10. Broadcast Layer

The protocol is transport-agnostic. Any mechanism that delivers events to all cluster members satisfies the broadcast requirement.

### 10.1 Requirements

- Deliver an event to all known peers
- Optionally collect acknowledgments from N peers before returning
- Fire-and-forget mode (min_acks=0) must not block the caller
- Idempotent delivery — receiving the same event twice is harmless (dedup handles it)

### 10.2 Implementations

| Transport | Peer Discovery | Latency | Scalability |
|-----------|---------------|---------|-------------|
| HTTP POST | Manual (config/API) | Network RTT | Moderate (10s of nodes) |
| UDP Gossip (SWIM) | Automatic | 1-2 gossip rounds | High (100s of nodes) |
| In-process channels | N/A | Microseconds | Testing only |

### 10.3 Persistence Notifications

After a batch write to the database succeeds, the node broadcasts a "persisted" message containing the list of `(origin_node_id, origin_seq)` keys that were written. Peers use this to update their unpersisted tracking — they know the event is durable somewhere.

## 11. Node Lifecycle

### 11.1 Startup (Existing Node)

1. Connect to database, run schema migrations
2. Load or create node identity from database
3. Load known peers from database
4. Rebuild in-memory caches from database:
   - Balances: aggregate `SUM(amount) GROUP BY bucket, account`
   - Heads: compute contiguous heads from event sequences
   - Origin-account mapping: `DISTINCT (origin_node_id, bucket, account)`
5. Derive `next_seq` from `MAX(origin_seq) + 1` for this node's events (crash safety)
6. Start background tasks (batch writer, orphan detector, catch-up sync)
7. Start serving

### 11.2 New Node Joining

Same as 11.1, plus after step 6:
- Join cluster via bootstrap peers (POST /join)
- Run trustless bootstrap: pull ALL events from peers, recompute state

### 11.3 Graceful Shutdown

1. Stop accepting new requests
2. Flush batch writer (write all buffered events to database)
3. Disconnect from peers
4. Shut down

### 11.4 Crash Recovery

On restart after a crash:
- Events in the batch writer buffer (not yet flushed) may be lost from this node's database
- Those events were broadcast to peers before the crash (broadcast happens before/alongside batch queue)
- Peers' orphan detectors will persist those events to their databases
- This node catches up from peers via catch-up sync or bootstrap
- Net result: zero events lost if at least one peer received the broadcast

## 12. Configuration

| Parameter | Default | Description |
|-----------|---------|-------------|
| `port` | — | HTTP listen port |
| `database_url` | — | Database connection string |
| `bootstrap` | [] | Peer addresses to join on startup |
| `max_peers` | 16 | Maximum tracked peers |
| `catchup_interval_ms` | 30000 | Catch-up sync interval |
| `batch_flush_interval_ms` | 100 | Batch writer flush interval |
| `batch_flush_size` | 1000 | Batch writer flush size threshold |
| `matview_refresh_ms` | 5000 | Balance summary refresh interval |
| `orphan_check_interval_ms` | 500 | Orphan detector scan interval |
| `orphan_age_ms` | 500 | Minimum age before an event is considered orphaned |
| `broadcast_mode` | http | Broadcast transport (http, gossip) |

## 13. Consistency Guarantees

| Property | Guarantee |
|----------|-----------|
| **Eventual consistency** | All nodes converge to the same state given sufficient sync time |
| **Per-origin ordering** | Events from a single origin are strictly ordered by sequence |
| **No global ordering** | Events from different origins have no guaranteed order |
| **At-least-once delivery** | Events may be delivered multiple times; dedup is idempotent |
| **Durability** | Events are durable once written to any node's database |
| **Availability** | Any node can accept writes independently, even during partitions |
| **Partition tolerance** | Nodes continue operating during network partitions; sync on reconnect |

This system provides AP (availability + partition tolerance) from the CAP theorem, sacrificing strong consistency for eventual consistency.
