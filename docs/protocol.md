# Distributed Append-Only Ledger Protocol

Version 1.5

## 1. Overview

A distributed system where multiple independent nodes accept credit/debit events for named accounts. Each node maintains a full replica of all events. Nodes are eventually consistent — any node can accept writes independently, and events propagate to all other nodes asynchronously.

No consensus protocol. No leader election. No shared storage. Each node is fully independent and can operate in isolation.

**Primary use case**: billing for LLM inference at the edge. Nodes are deployed globally (one per region), each handling per-completion charges for requests served locally. Balance and usage data sync across regions asynchronously. Temporary inconsistencies (e.g., a few cents of overdraft during sync lag) are acceptable; availability and low-latency writes are not negotiable.

## 2. Core Concepts

### 2.1 Event

The atomic unit of data. Immutable once created.

| Field | Type | Description |
|-------|------|-------------|
| `event_id` | string (UUID) | Globally unique identifier |
| `origin_node_id` | string | ID of the node that created this event |
| `origin_epoch` | uint32 | Restart epoch. Incremented each time the origin node starts. Starts at 1. |
| `origin_seq` | uint64 | Monotonically increasing sequence number, per origin node per epoch. Starts at 1, gapless within an epoch. |
| `created_at_unix_ms` | uint64 | Creation timestamp (milliseconds since Unix epoch) |
| `type` | string | Event type: `standard` or `void`. Default: `standard`. |
| `bucket` | string | Top-level namespace (e.g., tenant, environment) |
| `account` | string | Account within the bucket |
| `amount` | int64 | Positive = credit, negative = debit |
| `note` | string (nullable) | Optional human-readable description |
| `idempotency_nonce` | string (nullable) | Client-supplied deduplication nonce. Max 128 characters. |
| `void_ref` | string (nullable) | For `void` type events only: the `event_id` of the event being voided. |
| `hold_amount` | uint64 | Additional balance to reserve beyond the charge. Default: 0. |
| `hold_expires_at_unix_ms` | uint64 | Timestamp (ms since epoch) when the hold auto-releases. Default: 0. |

The tuple `(origin_node_id, origin_epoch, origin_seq)` is globally unique and serves as the deduplication key.

### 2.2 Event Types

| Type | Description |
|------|-------------|
| `standard` | A normal credit or debit. Created by clients. |
| `void` | A system-generated event that negates a duplicate. Created automatically during idempotency conflict resolution (§10). |

Both types are immutable, append-only, and replicated identically.

### 2.3 Node

An independent process with:
- A unique `node_id` (stable across restarts)
- Its own persistent storage (database)
- An HTTP API for clients and peer communication
- In-memory caches for fast reads

### 2.4 Balance

Two balance views exist for each `(bucket, account)` pair:

```
balance           = SUM(amount)  -- settled balance, all events
available_balance = balance - SUM(hold_amount WHERE hold_expires_at_unix_ms > now_ms)
                                 -- balance minus active holds
```

- **`balance`**: the true, settled balance. Sum of all `amount` values across all events from all origins. Used for usage history, reporting, auditing.
- **`available_balance`**: the effective spendable balance. Used by the overdraft guard (§9).

### 2.5 Contiguous Head

For each `(origin_node_id, origin_epoch)` pair, the highest sequence number N where all events 1 through N are present with no gaps. Used by the sync protocol to determine what events are missing.

A node may have events across multiple epochs for the same origin (one epoch per restart of that origin node). Heads are tracked independently per epoch.

### 2.6 Collapsed State

Per-account metadata indicating whether the balance is complete:
- **locally_confirmed**: every origin-epoch pair that has contributed events to this account has a contiguous head equal to its maximum known sequence. The balance is final given this node's current knowledge.
- **provisional**: at least one contributing origin-epoch pair has gaps (head < max known sequence). The balance may change when missing events arrive.

Note: "locally_confirmed" is scoped to this node's view. An origin this node hasn't heard from yet is not accounted for.

## 3. Event Lifecycle

### 3.1 Creation

A client sends a create request to any node. The receiving node:

1. **Checks idempotency** (if `idempotency_nonce` is present): if an event with the same `(idempotency_nonce, bucket, account, amount)` already exists, return the existing event without creating a new one (§10.3).

2. **Validates the overdraft guard** (debits only): if the projected available balance (`available_balance + amount`) would fall below the floor (`-max_overdraft` or 0), reject with an error. This check MUST be atomic with the balance update — implementations must use either:
   - A compare-and-swap (CAS) loop on the balance value: read current available balance, compute projected, if projected >= floor then atomically set the new balance, otherwise retry or reject. This allows lock-free concurrent credits while serializing competing debits on the same account.
   - A per-account mutex/lock: acquire lock, check balance, update, release. Simpler but higher contention.
   - A serialized write path (e.g., single-threaded event loop, actor model): all writes for a given account are processed sequentially. No concurrent debits possible.

   The critical invariant: between reading the balance and updating it, no other debit on the same account can interleave. Concurrent credits are safe (they only increase the balance).

3. **Assigns a sequence number**: the next value in this node's monotonic counter. Increment the counter.

4. **Generates a UUID** for the event_id.

5. **Sets hold metadata** (debits only): if the node is configured for balance holds (§11), set `hold_amount` and `hold_expires_at_unix_ms` on the event.

6. **Updates in-memory caches**: balance, available balance, active holds, contiguous head, origin tracking. These updates are the source of truth for subsequent requests — not the database.

7. **Queues the event for persistence** to the node's own database (asynchronous, not blocking the client).

8. **Broadcasts the event to all known peers** (asynchronous). If the client requested quorum acknowledgment (`min_acks > 0`), waits until the specified number of peers confirm receipt or a timeout expires.

9. **Returns the event** to the client along with the updated balance and acknowledgment info.

### 3.2 Replication

When a node receives an event from a peer (via broadcast or sync):

1. **Deduplication check**: if `origin_seq <= contiguous_head` for this `(origin_node_id, origin_epoch)`, or the event is already in the event buffer, discard as duplicate.

2. **Idempotency conflict check**: if the event has an `idempotency_nonce`, check for an existing event with the same `(idempotency_nonce, bucket, account, amount)`. If a conflict is detected, handle per §10.4.

3. **Update in-memory caches**: balance, available balance, active holds (if hold metadata is present), contiguous head (with gap tracking), origin tracking.

4. **Queue for persistence** to this node's own database.

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

1. Get the list of active peers from the node registry
2. For each peer, exchange node registries (merge any new entries)
3. For each peer, request their contiguous heads per `(origin, epoch)`
4. Compare with local heads — for ALL origin-epoch pairs in the registry, not just active peers
5. For each origin-epoch pair where a peer is ahead, request the missing event range
6. Insert received events locally (dedup handles any overlap)

**Parallel fetching**: when fetching a large missing range for an origin-epoch, the range MAY be split across multiple peers that have the data. For example, if origin-X epoch 3 is 10,000 events behind and peers A and C both have them, fetch sequences 1–5000 from A and 5001–10000 from C concurrently. Any peer that has a contiguous head >= the requested range can serve it. Dedup handles any overlap if ranges are conservatively split.

**Peer selection**: when multiple peers can serve the same range, prefer the peer with the lowest latency (from SWIM health tracking, §12.1). This naturally routes fetches to the nearest region.

**Interval**: configurable (default 30 seconds). This is NOT the primary sync mechanism.

### 4.3 Trustless Bootstrap

When a new node joins or a node restarts with empty/partial storage:

1. Connect to the cluster via bootstrap peers
2. Request node registries and heads from bootstrap peers
3. Merge received registries into local database
4. Build a fetch plan: for each origin-epoch pair, determine the full range needed (seq 1 to highest known head) and which peers can serve it
5. Execute the fetch plan in parallel: split ranges across peers by latency and availability. Multiple origin-epoch pairs can be fetched concurrently, and large ranges for a single pair can be split across peers (e.g., peer A serves seq 1–5000, peer C serves seq 5001–10000).
6. Insert all events into local storage and in-memory caches
7. Recompute balances from events (`SUM(amount) GROUP BY bucket, account`)
8. Start serving

Parallel bootstrap dramatically reduces cold-start time for a new node joining a cluster with a large event history. Dedup handles any overlap from conservative range splitting.

A new node NEVER trusts another node's balance values. It always recomputes from the full event log.

## 5. In-Memory State

Each node maintains these caches in memory for fast reads:

| Cache | Key | Value | Purpose |
|-------|-----|-------|---------|
| Balances | (bucket, account) | int64 (atomic) | Balance reads, reporting |
| Available Balances | (bucket, account) | computed | Overdraft checks |
| Active Holds | (bucket, account) | list of {hold_amount, hold_expires_at_unix_ms} | Available balance computation |
| Heads | (origin_id, epoch) | uint64 | Sync protocol, collapsed state |
| Account Origins | (bucket, account) | set of origin_ids | Collapsed state computation |
| Max Known Seqs | (origin_id, epoch) | uint64 | Collapsed state (detect gaps) |
| Event Buffer | (origin_id, epoch, seq) | Event | Orphan recovery, serve recent events |
| Unpersisted | (origin_id, epoch, seq) | timestamp | Track what's not yet in database |
| Pending Seqs | (origin_id, epoch) | sorted set of uint64 | In-memory head advancement |
| Idempotency Cache | (nonce, bucket, account, amount) | Event | Fast idempotency lookups |

### 5.1 Head Advancement

When an event arrives with sequence N for an `(origin_node_id, origin_epoch)`:
- If N == current_head + 1: advance the head to N. Then check the pending set — if N+1, N+2, ... are present, advance through them and remove from the pending set.
- If N > current_head + 1: this is out-of-order. Add N to the pending set. Do not advance the head.
- If N <= current_head: duplicate, discard.

Each epoch has its own independent head starting at 0. A new epoch from a known origin starts fresh — head 0, empty pending set.

This is purely in-memory. No database queries on the hot path.

### 5.2 Balance Updates

Balances are updated atomically (compare-and-swap for debits, simple add for credits) at the time the event is accepted into the in-memory cache. The database is NOT consulted for balance reads or writes on the hot path.

On startup, balances are rebuilt from the database.

### 5.3 Hold Expiry

Periodically sweep the Active Holds lists and remove expired entries. This is an optimization — expired holds are excluded from `available_balance` by the time check regardless, but cleanup prevents unbounded memory growth.

## 6. Persistence Layer

### 6.1 Database Schema

**events** — append-only event log:
- Primary key: `event_id`
- Unique constraint: `(origin_node_id, origin_epoch, origin_seq)` — the dedup key
- Columns: all fields from §2.1
- Indexes:
  - `(bucket, account)` for balance aggregation
  - `(created_at_unix_ms)` for time-ordered queries
  - `(idempotency_nonce, bucket, account, amount)` WHERE `idempotency_nonce IS NOT NULL` — for conflict detection

**node_meta** — this node's identity:
- `node_id` (primary key), `host`, `port`, `current_epoch`, `next_seq`

**node_registry** — permanent record of all nodes that have ever existed in the cluster:
- `node_id` (primary key)
- `addr` (string) — last known address (host:port)
- `first_seen_at_unix_ms` (uint64) — when this node was first discovered
- `last_seen_at_unix_ms` (uint64) — when this node last communicated successfully
- `status` (string) — `active`, `suspect`, `unreachable`, `decommissioned`
- Rows are NEVER deleted. A node that goes offline is marked `unreachable`, not removed.
- Replicated to all nodes via the join handshake and catch-up sync (§14).

**balance_summary** (OPTIONAL optimization) — cached balance view:
- `bucket`, `account`, `balance` (= SUM(amount) from events)
- Refreshed periodically (default every 5 seconds)
- Purpose: startup optimization for large event tables. Instead of scanning millions of events to compute `SUM(amount) GROUP BY bucket, account`, read the pre-computed view.
- NOT the source of truth. On startup, if the view is stale or missing, fall back to the full aggregate query. The in-memory balance cache is always rebuilt from events, not trusted from this view.
- Implementations with small event tables can skip this entirely.

### 6.2 Conflict Handling

All inserts use conflict-skip semantics on `(origin_node_id, origin_epoch, origin_seq)`. If a row already exists with the same key, the insert is silently skipped.

For integrity verification: if a conflict is detected, the existing row's `event_id` should be compared with the incoming event's `event_id`. If they differ, this indicates data corruption — log a warning. (Sequence reuse across epochs is impossible by construction; within an epoch it indicates a bug.)

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
  "idempotency_nonce": "string | null",
  "max_overdraft": 0,
  "min_acks": 0,
  "ack_timeout_ms": 500
}
```

| Field | Default | Description |
|-------|---------|-------------|
| `idempotency_nonce` | null | Client-supplied deduplication nonce. Combined with `bucket`, `account`, and `amount` to form the idempotency key. |
| `max_overdraft` | 0 | How far below zero the balance can go. 0 = no overdraft. |
| `min_acks` | 0 | Minimum peer acknowledgments before responding. 0 = fire-and-forget. |
| `ack_timeout_ms` | 500 | Maximum time to wait for acks (ms). |

Response (201 — created):
```json
{
  "event": { ... },
  "balance": 0,
  "available_balance": 0,
  "deduplicated": false,
  "acks": { "received": 0, "requested": 0, "timeout": false }
}
```

Response (200 — deduplicated):
```json
{
  "event": { ... },
  "balance": 0,
  "available_balance": 0,
  "deduplicated": true,
  "acks": null
}
```

Response (422 — overdraft rejected):
```json
{
  "error": "insufficient_funds",
  "balance": 0,
  "available_balance": 0,
  "projected_balance": 0,
  "limit": 0
}
```

**GET /health** — Node status
**GET /state** — Full state (heads, checksum, peers, event count, total balance)
**GET /events** — All events (sorted by timestamp, from database)
**GET /heads** — Contiguous head per (origin, epoch)
**GET /balances** — All account balances (settled and available)
**GET /collapsed** — Balance + sync completeness per account
**GET /collapsed/:bucket/:account** — Single account collapsed state
**GET /persistence** — Count and age of unpersisted events
**GET /debug/origin/:id** — Sequences and gaps for a specific origin

### 7.2 Peer Endpoints

**POST /events/replicate** — Accept a replicated event from a peer

Request: an Event object.
Response: `{"status": "ok", "inserted": true|false}`

**POST /events/range** — Fetch events for a specific origin epoch in a sequence range

Request:
```json
{
  "origin_node_id": "string",
  "origin_epoch": 0,
  "from_seq": 0,
  "to_seq": 0
}
```

Response: `[Event, Event, ...]`

**POST /join** — Cluster join handshake

Request: `{"node_id": "string", "addr": "string"}`
Response:
```json
{
  "node_id": "string",
  "addr": "string",
  "registry": [
    {"node_id": "string", "addr": "string", "status": "string", "first_seen_at_unix_ms": 0, "last_seen_at_unix_ms": 0},
    ...
  ],
  "heads": {...}
}
```

The response includes the full node registry. The joining node merges it with its own registry and begins syncing events from all origins.

**GET /registry** — Full node registry (all nodes that have ever existed)
**POST /registry/decommission** — Mark a node as decommissioned (operator action)

## 8. Convergence Verification

### 8.1 Head Comparison (Primary, O(origin-epochs))

The primary convergence check. Compare contiguous heads per `(origin_node_id, origin_epoch)` between two nodes. If all heads match for all origin-epoch pairs, the nodes have the same contiguous event prefixes. This is O(number of origin-epoch pairs) — fast regardless of event count.

At 200 nodes with infrequent restarts, the number of origin-epoch pairs stays manageable (a node that has restarted 10 times contributes 10 pairs).

Limitation: heads only prove prefix equality. They don't detect corrupted events within the prefix (same sequence, different payload).

### 8.2 Full Checksum (Audit, O(events))

For periodic auditing or when head comparison is insufficient.

Canonical format per event:
```
{origin_node_id}:{origin_epoch}:{origin_seq}:{event_id}:{type}:{bucket}:{account}:{amount}:{void_ref}:{idempotency_nonce}:{hold_amount}:{hold_expires_at_unix_ms}
```

Nullable fields use the empty string when null.

Order by `(origin_node_id ASC, origin_epoch ASC, origin_seq ASC)`. Join with `\n`. SHA-256, hex-encoded lowercase (64 chars). Excludes `note` (cosmetic).

**Warning**: this scans all events. At millions of events, this is expensive. Use sparingly (e.g., daily audit, not per-sync-cycle).

### 8.3 Incremental Verification (Recommended for Scale)

For large event tables, maintain per-origin-epoch rolling hash digests:

**Rolling prefix digest**: a single 32-byte hash per `(origin_node_id, origin_epoch)`, updated incrementally:
```
prefix_digest[0] = HASH("")
prefix_digest[n] = HASH(prefix_digest[n-1] || event_hash(n))
```
Where `event_hash(n)` is the SHA-256 of the canonical event string.

- **Update cost**: O(1) per new contiguous event
- **Compare cost**: O(origin-epoch pairs) — compare `(head, prefix_digest)` per pair
- **Storage**: 32 bytes per origin-epoch pair

If two nodes have the same `(head, prefix_digest)` for an origin-epoch pair, their contiguous prefixes for that epoch are cryptographically identical.

**Block digests** (optional, for locating divergence): store a digest per block of N events (e.g., every 4096 sequences). If prefix digests differ, compare block digests to narrow the mismatch to a specific range without scanning all events. Storage: ~32 bytes per block.

These incremental approaches are OPTIONAL optimizations. Implementations may start with head comparison only and add digest layers when event counts warrant it.

## 9. Overdraft Guard

### 9.1 Behavior

The overdraft guard prevents a single node from allowing debits that would push an account's available balance below a configurable floor. It is checked atomically at event creation time.

- **Credits** (amount > 0): always succeed, no guard applied
- **Debits** (amount < 0): `available_balance + amount >= -max_overdraft` must hold
- **max_overdraft = 0**: available balance cannot go below 0
- **max_overdraft = 500**: available balance can go as low as -500

Where `available_balance = balance - active_holds` (§11.3).

### 9.2 Limitations

The guard is LOCAL to the node receiving the request. It does not consult other nodes. In a multi-node scenario:
- Node A sees available balance 1000, allows debit -800 → available balance 200
- Node B also sees available balance 1000 (hasn't synced A's debit yet), allows debit -800 → available balance 200
- After sync: true balance = 1000 - 800 - 800 = -600 (overdraft!)

This is a known, documented limitation. The guard reduces the probability and magnitude of overdrafts but does not eliminate them in a distributed setting. Eliminating them would require consensus (e.g., Raft, Paxos), which this protocol explicitly avoids for performance and availability.

Balance holds (§11) further reduce the overdraft window by reserving balance across nodes.

For use cases requiring strict overdraft prevention, route all debits for a given account to a single node.

## 10. Client Idempotency

### 10.1 Idempotency Key

Events may include an optional client-supplied nonce for safe retries. The idempotency identity of an event is the composite key:

```
(idempotency_nonce, bucket, account, amount)
```

Two events with the same composite key are considered duplicates of the same operation. Two events with the same nonce but different amounts are distinct operations — not duplicates.

The `idempotency_nonce` is part of the immutable event record, set at creation time, replicated as-is.

### 10.2 Rationale

Clients calling `POST /events` may experience timeouts where the event was created but the response was lost. Without idempotency, a retry creates a duplicate charge. The idempotency nonce allows safe retries: if the nonce matches, the original event is returned without creating a new one.

Including `amount` in the composite key ensures that a nonce reused with a different amount is treated as a new operation, not a duplicate.

### 10.3 Local Enforcement

On the originating node, idempotency is enforced at event creation time:

1. **In-memory check**: maintain a map of `(idempotency_nonce, bucket, account, amount) → event` for recent events. If the key exists, return the original event and balance — do not create a new event.

2. **Database fallback**: if the in-memory cache has been evicted (e.g., after restart), query the events table for a matching composite key. If found, return the existing event.

3. **Response**: a deduplicated request returns the original event (HTTP 200, not 201).

The in-memory cache may be bounded (e.g., LRU with TTL of 24 hours). After eviction, the database is the backstop.

### 10.4 Cross-Node Conflict Resolution

Two nodes may independently accept events with the same composite idempotency key before sync propagates. This produces two distinct events (different `event_id`, different `origin_node_id`) with the same logical identity.

**Detection**: when a node receives a replicated event, it checks whether a local event with the same `(idempotency_nonce, bucket, account, amount)` already exists. If so, this is an idempotency conflict.

**Winner determination — oldest wins, deterministic**:

1. Lower `created_at_unix_ms` wins.
2. If timestamps are equal: lower `event_id` (lexicographic comparison) wins.

All nodes apply the same rule and agree on the winner.

### 10.5 Void Emission

Any node that detects an idempotency conflict emits a void event for the loser. The void is placed on the emitting node's own sequence.

When a node detects a conflict (either its own event lost, or it sees an unresolved conflict for a different origin):

1. Create a new `void` event on this node's own sequence:
   - `type`: `void`
   - `bucket`, `account`: same as the voided event
   - `amount`: negation of the voided event's amount (e.g., if the voided event was `-50`, the void event is `+50`)
   - `void_ref`: the `event_id` of the voided (losing) event
   - `idempotency_nonce`: `"void:{loser_event_id}"` — deterministic, derived from the event being voided
   - `hold_amount`: 0 (no hold on void events)
   - `note`: human-readable reason, e.g., `"void: duplicate of event {winner_event_id}"`

2. Before emitting, check whether an event with idempotency key `("void:{loser_event_id}", bucket, account, amount)` already exists locally (in-memory or database). If it does, do not emit.

3. Broadcast the void event to all peers like any other event.

### 10.6 Multiple Void Emission (Cascade)

Because broadcast latency between global nodes can be 2–300ms, multiple nodes may independently emit void events for the same loser before hearing about each other's voids. This is safe.

All such voids share the same idempotency nonce `"void:{loser_event_id}"`, so they are themselves duplicates. The idempotency mechanism resolves them recursively: the oldest void wins, and the losing voids are voided in turn. Each void-of-void has a unique `idempotency_nonce` (`"void:{losing_void_event_id}"`), so no further cascade occurs.

**Example** — node B dies, nodes A, C, D all emit voids:

```
-- Original conflict
A:205  standard  -50  nonce="completion:abc"         ← winner
B:310  standard  -50  nonce="completion:abc"         ← loser

-- Three nodes emit voids (same nonce, so they conflict with each other)
A:206  void  +50  void_ref=B:310  nonce="void:{B:310.id}"   ← oldest, wins
C:891  void  +50  void_ref=B:310  nonce="void:{B:310.id}"   ← duplicate void
D:444  void  +50  void_ref=B:310  nonce="void:{B:310.id}"   ← duplicate void

-- Losing voids get voided (unique nonces, no further cascade)
C:892  void  -50  void_ref=C:891  nonce="void:{C:891.id}"
D:445  void  -50  void_ref=D:444  nonce="void:{D:444.id}"

SUM = -50 ✓
```

In general, N nodes emitting voids for the same loser produces `2N - 3` correction events beyond the single void needed. For a 3–5 node cluster, this is a handful of extra events for a rare scenario (cross-region idempotency conflict with a dead originator). The balance converges correctly regardless.
### 10.7 Balance Model

Balance remains a pure sum over all events:

```
balance(bucket, account) = SUM(amount) for all events WHERE bucket = b AND account = a
```

This includes both `standard` and `void` events. Void events have negating amounts, so they cancel the duplicate's effect in the sum.

Example:
```
Event 205 (node-A, seq 205): type=standard, amount=-50, nonce="completion:abc"   → balance: -50
Event 310 (node-B, seq 310): type=standard, amount=-50, nonce="completion:abc"   → balance: -100 (temporary)
Event 524 (node-B, seq 524): type=void, amount=+50, void_ref=event_310_id       → balance: -50  (correct)
```

No derived state. No mutable flags. The log is a set of immutable entries whose sum converges to the correct balance.

### 10.8 Consistency Window

Between conflict detection and void propagation, the balance is temporarily incorrect (double-charged). The duration of this window is bounded by broadcast latency (typically < 100ms within a region) or catch-up sync interval (default 30 seconds).

### 10.9 Operational Notes

- **Nonce generation**: clients SHOULD derive the nonce deterministically from the operation (e.g., `completion:{request_id}`). Random nonces per attempt defeat the purpose.
- **Retry target**: clients SHOULD retry against the same node/region to avoid cross-node conflicts entirely.
- **Events without nonces**: events with `idempotency_nonce = null` bypass idempotency checks. They are never considered duplicates.
- **Void events in usage history**: void events are visible to users in their usage history, clearly marked with `type=void` and a reference to the original event. This provides full auditability.

## 11. Balance Holds

### 11.1 Purpose

When a node accepts a debit, it reserves an additional amount of balance for a configurable duration. This reduces the available balance visible to all nodes, preventing other nodes from spending against balance that is likely to be consumed by further requests arriving at the originating node.

This is a soft distributed lock — it does not require consensus and does not guarantee prevention of overspend, but it significantly reduces the overdraft window in multi-node scenarios.

### 11.2 Hold Lifecycle

1. **Creation**: when a node creates a debit event, it sets `hold_amount` and `hold_expires_at_unix_ms` based on node configuration.

2. **Propagation**: the hold fields travel with the event via broadcast and sync. All nodes that receive the event include the hold in their `available_balance` computation.

3. **Expiry**: holds expire passively. No event is emitted on expiry. Each node independently stops including the hold in `available_balance` once `now_ms >= hold_expires_at_unix_ms`. Since all nodes use the same expiry timestamp from the event, they converge (modulo clock skew).

4. **No early release**: holds cannot be released early. They expire at the specified time. This keeps the model simple — no mutable state, no release events.

### 11.3 Available Balance Computation

```
active_holds      = SUM(hold_amount) for events WHERE hold_expires_at_unix_ms > now_ms
                                                  AND bucket = b AND account = a
available_balance = balance - active_holds
```

The overdraft guard (§9.1) checks against `available_balance`:
```
available_balance + amount >= -max_overdraft
```

### 11.4 Hold Sizing

The hold amount and duration are configured per-node:

| Parameter | Default | Description |
|-----------|---------|-------------|
| `hold_multiplier` | 10 | Hold amount = `abs(charge_amount) × hold_multiplier`. |
| `hold_duration_ms` | 600000 | Hold duration (default 10 minutes). |

Example: a charge of `-100` (1 USD in cents) with `hold_multiplier=10` creates a hold of `1000` (10 USD) expiring 10 minutes from now.

Nodes MAY use different hold configurations based on regional traffic patterns.

### 11.5 Interaction with Holds from Other Nodes

When a node receives a replicated event with hold metadata:

1. Add the hold to the in-memory Active Holds cache for the relevant `(bucket, account)`.
2. The hold immediately reduces `available_balance` on this node.
3. Subsequent local debit requests for this account will see the reduced available balance and may be rejected by the overdraft guard.

This is the mechanism by which one node's spending activity reduces available balance on other nodes without consensus.

### 11.6 Limitations

- **Clock skew**: nodes with skewed clocks will disagree on whether a hold is active. For holds measured in minutes, clock skew of seconds is negligible.
- **Stale holds**: if broadcast is delayed, a node may not know about a peer's holds until catch-up sync. During this window, the node's `available_balance` is higher than it should be.
- **Over-reservation**: aggressive hold multipliers reduce available balance significantly, potentially rejecting legitimate debits. Tune `hold_multiplier` and `hold_duration_ms` based on actual traffic patterns.
- **No early release**: if a user stops making requests, the held balance remains unavailable until expiry. This is a UX tradeoff for simplicity.

## 12. Broadcast and Membership Layer

The protocol uses **SWIM** (Scalable Weakly-consistent Infection-style Membership) as the foundation for both membership management and event dissemination.

### 12.1 SWIM Overview

SWIM provides two services in a single protocol:

**Failure detection**: each node periodically (every `swim_probe_interval_ms`) picks a random peer and sends a **ping**. If no **ack** arrives within a timeout, the node asks `swim_indirect_probes` other random peers to send **ping-req** (indirect probes) to the suspect. If none respond, the target is marked **suspect**, then **dead** after `swim_suspicion_timeout_ms`.

**Membership dissemination**: membership state changes (join, suspect, dead, alive) are piggybacked on ping/ack/ping-req messages as extra payload. Updates spread epidemically through the cluster in O(log N) protocol rounds. No separate broadcast channel needed for membership.

### 12.2 Event Dissemination

Events are disseminated via a **gossip layer** that runs alongside SWIM membership. When a node creates or receives a new event, it enqueues the event into the gossip broadcast buffer. The event is piggybacked on outgoing SWIM protocol messages (pings, acks) and also sent via dedicated gossip messages to random peers.

For high-throughput scenarios where piggyback bandwidth is insufficient, nodes fall back to direct HTTP POST for event replication:

| Method | When | Latency | Overhead |
|--------|------|---------|----------|
| Gossip piggyback | Default for small events at moderate throughput | O(log N) rounds | Minimal — reuses SWIM traffic |
| Dedicated gossip messages | When piggyback buffer is full | O(log N) rounds | Separate UDP messages |
| Direct HTTP POST | Quorum acks (`min_acks > 0`), large event batches, or when gossip is too slow | 1 RTT to target | One connection per peer |

### 12.3 Quorum Acknowledgments

When a client requests `min_acks > 0`, gossip alone cannot provide timely confirmation. The node sends the event via direct HTTP POST to peers and waits for the requested number of acks or a timeout.

### 12.4 Persistence Notifications

After a batch write to the database succeeds, the node disseminates a "persisted" notification containing the list of `(origin_node_id, origin_epoch, origin_seq)` keys that were written. This is sent via gossip (piggybacked or dedicated) — it is distributed best-effort, not latency-critical.

### 12.5 Implementation

In Rust, use **foca** (`https://github.com/caballero-io/foca`) — a SWIM implementation designed as a library. It handles the protocol state machine (probe, suspect, dead transitions) and provides hooks for custom dissemination payloads.

The node wraps foca with:
- A UDP transport layer for SWIM protocol messages
- A gossip broadcast buffer for event dissemination
- An HTTP fallback for quorum acks and large payloads
- Integration with the node registry (§14) — SWIM membership events trigger registry updates

### 12.6 Configuration

| Parameter | Default | Description |
|-----------|---------|-------------|
| `swim_port` | `port + 1` | UDP port for SWIM protocol |
| `swim_probe_interval_ms` | 1000 | How often to probe a random peer |
| `swim_probe_timeout_ms` | 500 | Time to wait for a direct ping ack |
| `swim_indirect_probes` | 3 | Number of peers asked to indirect-probe a suspect |
| `swim_suspicion_timeout_ms` | 5000 | Time in suspect state before declaring dead |
| `swim_gossip_fanout` | 3 | Number of peers to send each gossip message to |

## 13. Node Lifecycle

### 13.1 Startup (Existing Node)

1. Connect to database, run schema migrations
2. Load or create node identity from database
3. Increment `current_epoch` in database, set `next_seq = 1` for the new epoch
4. Load node registry from database
5. Rebuild in-memory caches from database:
   - Balances: aggregate `SUM(amount) GROUP BY bucket, account`
   - Active holds: load unexpired holds from events
   - Heads: compute contiguous heads from event sequences per `(origin, epoch)` for ALL origins in registry
   - Origin-account mapping: `DISTINCT (origin_node_id, bucket, account)`
   - Idempotency cache: recent events with non-null nonces
6. Join the SWIM cluster via bootstrap peers — begin failure detection and gossip
7. Merge SWIM's live membership into the node registry
8. Start background tasks (batch writer, orphan detector, catch-up sync)
9. Start serving

### 13.2 New Node Joining

Same as 13.1, plus after step 8:
- Receive full node registry from bootstrap peer via `POST /join`
- Merge received registry into local database (covers historical origins SWIM doesn't know about)
- Run trustless bootstrap: pull ALL events from ALL origins in the registry, recompute state
- **Readiness gate**: do not mark as healthy or accept client traffic until local heads are within a configurable threshold of peers' heads. This prevents a cold node from approving debits against a stale balance.

### 13.3 Graceful Shutdown

1. Stop accepting new requests
2. Flush batch writer (write all buffered events to database)
3. Leave the SWIM cluster (sends departure notification to peers)
4. Shut down

### 13.4 Crash Recovery

On restart after a crash:
- Events in the batch writer buffer (not yet flushed) are lost from THIS node's database.
- Those events were broadcast to peers before the crash (broadcast happens before batch queue).
- Peers' orphan detectors persist those events to their databases.
- The lost events belong to the previous epoch. They will be synced back during catch-up.

**Epoch-based recovery**: the node increments `current_epoch` in the database and starts `origin_seq` at 1 for the new epoch. There is no risk of sequence reuse — the new epoch is a distinct sequence space. Events from the previous epoch (including any that were broadcast but not flushed locally) are recovered from peers during normal catch-up sync.

**Startup sequence**:
1. Read own DB: get `current_epoch` → increment to `current_epoch + 1`, persist.
2. Set `next_seq = 1` for the new epoch.
3. Join SWIM cluster, begin catch-up sync (recovers unflushed events from prior epoch).
4. Resume serving immediately — no need to wait for catch-up to complete before accepting writes, because the new epoch guarantees no sequence collision.

This is the primary advantage of the epoch mechanism: a crashed node can resume serving writes immediately without waiting for peer sync. Read consistency still requires catch-up (the node's balances may be stale until prior-epoch events are recovered), so the readiness gate (§13.2) still applies for accuracy-sensitive reads.

## 14. Node Registry and Peer Management

### 14.1 Node Registry (Permanent)

Every node that has ever existed in the cluster is recorded in a permanent **node registry** on every node's database. Registry entries are NEVER deleted.

**Hard requirement**: every node MUST eventually know about every other node that has ever existed. The event log is only complete when a node has synced events from ALL origins. A node missing a registry entry is missing an entire origin's events with no way to detect the gap.

A registry entry contains:
- `node_id` — unique, stable identifier
- `addr` — last known address (host:port)
- `first_seen_at_unix_ms` — when this node was first discovered
- `last_seen_at_unix_ms` — when this node last communicated successfully
- `status` — `active`, `suspect`, `unreachable`, or `decommissioned`

### 14.2 SWIM-Driven Membership

SWIM (§12.1) is the primary source of membership information. The node registry is the persistent store that records SWIM's view of the world.

**SWIM → Registry integration**:

| SWIM Event | Registry Action |
|------------|----------------|
| New node joins (alive) | Insert with `status = active`, begin syncing events from this origin |
| Node confirmed alive (ack) | Update `last_seen_at_unix_ms`, set `status = active` |
| Node suspected (no ack) | Set `status = suspect` |
| Node declared dead | Set `status = unreachable` |
| Previously dead node rejoins | Set `status = active`, update `addr` if changed |

SWIM membership events propagate automatically via the gossip protocol — no explicit broadcast of registry entries is needed. Every node running SWIM converges to the same membership view within O(log N) protocol rounds.

### 14.3 Registry Propagation (Safety Net)

SWIM handles real-time membership. The following mechanisms ensure the permanent registry converges for historical origins that predate a node's join:

**Catch-up sync**: during every catch-up sync cycle (§4.2), nodes exchange their full registries. Entries are merged by `node_id` — if both sides have an entry, keep the one with the later `last_seen_at_unix_ms`. This ensures a new node learns about origins from long-dead nodes that SWIM would not know about.

**Join handshake**: `POST /join` response includes the full node registry. A new node gets the complete registry from its bootstrap peer on first contact, covering all historical origins.

**Consistency check**: during catch-up sync, a node can verify registry completeness by comparing its set of known `origin_node_id` values from the events table against its registry. Any origin present in events but missing from the registry indicates a bug.

### 14.4 Active Peer Set

The **active peer set** is derived directly from SWIM's live membership view — all nodes that SWIM considers `alive`. This replaces manual peer tracking.

At scale (200+ nodes), not every live node needs to receive direct event broadcasts. The active peer set for broadcast purposes is bounded by `max_peers` (default 16). Selection criteria when the live set exceeds `max_peers`:

- **Geographic diversity**: prefer at least one peer per region to ensure cross-region propagation
- **Latency**: prefer peers with lower round-trip times
- **SWIM probes handle the rest**: nodes not in the active broadcast set still receive events via gossip dissemination (O(log N) hops)

A node NOT in the active peer set still has its events synced — during catch-up sync, the node fetches events for ALL origins in the registry, not just active peers.

### 14.5 Status Transitions

```
                     ┌─────────────────────────┐
                     │                         │
                     ▼                         │
  ┌──────┐    ┌──────────┐    ┌─────────────┐  │
  │ join │───▶│  active   │───▶│   suspect   │──┘
  └──────┘    └──────────┘    └─────────────┘
                   ▲                │
                   │                ▼
                   │         ┌─────────────┐
                   └─────────│ unreachable │
                             └─────────────┘
                                    │
                                    ▼ (manual only)
                             ┌─────────────────┐
                             │ decommissioned   │
                             └─────────────────┘
```

- `active → suspect`: SWIM probe failure (no ack, indirect probes failed)
- `suspect → active`: SWIM receives ack (alive confirmation)
- `suspect → unreachable`: SWIM suspicion timeout expires
- `unreachable → active`: node rejoins the SWIM cluster
- `active/unreachable → decommissioned`: manual operator action only. A decommissioned node is never contacted again but its events remain in the log and registry.

Unreachable nodes remain in the registry permanently. Their events are still fetched during catch-up sync from any peer that has them.

### 14.6 Self-Exclusion

A node MUST NOT add itself to its own active peer set. The node's own `node_id` is excluded when deriving the active set.

## 15. Configuration

| Parameter | Default | Description |
|-----------|---------|-------------|
| `port` | — | HTTP listen port |
| `database_url` | — | Database connection string |
| `bootstrap` | [] | Peer addresses to join SWIM cluster on startup |
| `max_peers` | 16 | Maximum active peers for direct broadcast (registry is unbounded) |
| `catchup_interval_ms` | 30000 | Catch-up sync interval |
| `sync_fetch_concurrency` | 4 | Max concurrent range fetches during catch-up sync and bootstrap |
| `batch_flush_interval_ms` | 100 | Batch writer flush interval |
| `batch_flush_size` | 1000 | Batch writer flush size threshold |
| `matview_refresh_ms` | 5000 | Balance summary refresh interval |
| `orphan_check_interval_ms` | 500 | Orphan detector scan interval |
| `orphan_age_ms` | 500 | Minimum age before an event is considered orphaned |
| `hold_multiplier` | 10 | Hold amount = abs(charge) × multiplier |
| `hold_duration_ms` | 600000 | Hold duration (10 minutes) |
| `readiness_head_lag` | 100 | Max event lag behind peers before marking ready |

SWIM-specific parameters are in §12.6.

## 16. Consistency Guarantees

| Property | Guarantee |
|----------|-----------|
| **Eventual consistency** | All nodes converge to the same state given sufficient sync time |
| **Per-origin ordering** | Events from a single origin are strictly ordered by (epoch, sequence) |
| **No global ordering** | Events from different origins have no guaranteed order |
| **At-least-once delivery** | Events may be delivered multiple times; dedup is idempotent |
| **Durability** | Events are durable once written to any node's database |
| **Availability** | Any node can accept writes independently, even during partitions |
| **Partition tolerance** | Nodes continue operating during network partitions; sync on reconnect |
| **Immutability** | Events are never modified or deleted. Corrections are made by appending void events. |
| **Commutative balance** | Balance = SUM(amount) over all events. Order of event arrival does not affect final balance. |

This system provides AP (availability + partition tolerance) from the CAP theorem, sacrificing strong consistency for eventual consistency.

## 17. Open Issues and Future Work

- **Strict overdraft enforcement**: requires consensus (e.g., Raft) or routing all debits for an account to a single node. Documented as a known limitation (§9.2).
- **Rolling prefix digests**: incremental convergence verification (§8.3) is recommended but not required by this protocol version.
- **Event signing**: no cryptographic signatures on events. In an untrusted environment, a malicious node can forge events with any `origin_node_id`. Adding ed25519 signatures per event would prevent this.
- **Rate limiting**: no built-in mechanism. Implementations should add per-client or per-account rate limits at the API layer.
- **Hold tuning**: optimal `hold_multiplier` and `hold_duration_ms` values depend on traffic patterns. Future versions may support adaptive holds based on observed request rates.
- **Log compaction**: for deployments with very high event volume, a snapshotting and truncation mechanism may be needed. Not addressed in this version.
- **SWIM protocol tuning**: default SWIM parameters (§12.6) are suitable for clusters of 5–50 nodes. At 200+ nodes, `swim_probe_interval_ms`, `swim_suspicion_timeout_ms`, and `swim_gossip_fanout` may need tuning to balance detection speed against false-positive rates across high-latency global links.
- **Epoch proliferation**: frequent restarts produce many origin-epoch pairs, increasing the size of head comparison (§8.1) and sync plan computation. At 200 nodes with 100 restarts each, the head table has 20,000 entries. Manageable, but implementations may want to coalesce finalized epochs (where the head is known to be complete and no new events will arrive) into a single digest for comparison efficiency.
