# Distributed Append-Only Ledger Protocol

Version 1.9 (per-bucket seq/epoch identity — see §2.1, §13.1)

---

### v1.9 — Per-bucket identity (2026-04-22)

Event identity changed from the 3-tuple `(origin_node_id, origin_epoch,
origin_seq)` to the 4-tuple `(bucket, origin_node_id, origin_epoch,
origin_seq)`. Each `(bucket, origin_node_id)` pair owns an independent
epoch+seq line; a bucket is bumped lazily on the first write to it after
startup (§13.1). Rationale: in v1.8 a single stale event on one bucket
(e.g., an orphaned tombstone left over from a removed bucket-purge
feature) could stall the whole node's `sync_gap` metric, because
`sync_gap` was `max` over every `(origin, epoch)` entry in memory. With
per-bucket seq spaces, anomalies in bucket A cannot influence bucket B's
catch-up or sync-gap reporting; and buckets we never write to never
accumulate empty epochs across restarts.

The wire protocols (`/shardd/heads/`, `/shardd/range/`) bumped to `/2`.
The `HeadsResponse` key format is now `"{bucket}\t{origin}:{epoch}"`.
`RangeRequest` carries a `bucket` field. The storage schema's dedup index
is now `(bucket, origin_node_id, origin_epoch, origin_seq)` and there's
a new `bucket_seq_allocator` table. `node_meta` no longer stores a
`current_epoch` or `next_seq` — both are per-bucket now.

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
| `origin_epoch` | uint32 | Per-`(bucket, origin_node_id)` epoch (§13.1). Bumped lazily on the first write to the bucket after startup. Buckets the node never writes to after a restart do not bump. Starts at 1. |
| `origin_seq` | uint64 | Monotonically increasing sequence number, per-`(bucket, origin_node_id, origin_epoch)`. Starts at 1, gapless within an epoch. Two different buckets written by the same node have independent seq spaces. |
| `created_at_unix_ms` | uint64 | Creation timestamp (milliseconds since Unix epoch) |
| `type` | string | Event type: `standard`, `void`, or `hold_release`. Default: `standard`. |
| `bucket` | string | Top-level namespace (e.g., tenant, environment) |
| `account` | string | Account within the bucket |
| `amount` | int64 | Positive = credit, negative = debit |
| `note` | string (nullable) | Optional human-readable description |
| `idempotency_nonce` | string (required) | Client-supplied deduplication nonce — every event carries one. Max 128 characters. A retry of the same logical operation must reuse the same nonce. |
| `void_ref` | string (nullable) | For system-generated correction events (`void`, `hold_release`) only: the `event_id` of the event being corrected. |
| `hold_amount` | uint64 | For held debit `standard` events only: additional balance to reserve beyond the charge. Default: 0. |
| `hold_expires_at_unix_ms` | uint64 | For held debit `standard` events: timestamp (ms since epoch) when the hold auto-releases. Default: 0. |

The tuple `(bucket, origin_node_id, origin_epoch, origin_seq)` is globally unique and serves as the deduplication key. Two nodes writing to the same bucket still produce distinct identities because `origin_node_id` is part of the tuple — no cross-node coordination is required.

### 2.2 Event Types

| Type | Description |
|------|-------------|
| `standard` | A normal credit or debit. Created by clients. |
| `void` | A system-generated event that negates a duplicate. Created automatically during idempotency conflict resolution (§10). |
| `hold_release` | A system-generated event that cancels a debit's hold without changing settled balance. Used when a held debit is later corrected (§10, §11). |
| `bucket_delete` | Admin/owner-triggered hard-delete instruction. Only valid when `bucket = "__meta__"`; the target bucket to nuke is carried in the `account` field. See §3.5. |

All three event types are immutable, append-only, and replicated identically.

### 2.3 Node

An independent process with:
- A unique `node_id` (stable across restarts)
- Its own persistent storage (database)
- Libp2p networking for peer communication
- In-memory caches for fast reads

### 2.4 Balance

Two balance views exist for each `(bucket, account)` pair:

```
balance           = SUM(amount)          -- settled balance, all events
active_holds      = SUM(effective_hold)  -- active holds after corrections (§11.3)
available_balance = balance - active_holds
```

- **`balance`**: the true, settled balance. Sum of all `amount` values across all events from all origins. Used for usage history, reporting, auditing.
- **`available_balance`**: the effective spendable balance. Used by the overdraft guard (§9).

### 2.5 Contiguous Head

For each `(bucket, origin_node_id, origin_epoch)` triple, the highest
sequence number N where all events 1 through N are present with no gaps.
Used by the sync protocol to determine what events are missing.

A node may have events across multiple epochs for the same
`(bucket, origin)` pair (one per lazy bump after a restart that actually
wrote to that bucket — see §13.1). Heads are tracked independently per
triple; a gap in `(bucket-A, origin, epoch)` has no effect on the head
for `(bucket-B, origin, epoch)` even when the origin is the same.

### 2.6 Collapsed State

Per-account metadata indicating whether the balance is complete:
- **locally_confirmed**: every `(bucket, origin, epoch)` triple that has contributed events to this account has a contiguous head equal to its maximum known sequence. The balance is final given this node's current knowledge.
- **provisional**: at least one contributing triple has gaps (head < max known sequence). The balance may change when missing events arrive.

Note: "locally_confirmed" is scoped to this node's view. An origin this node hasn't heard from yet is not accounted for.

## 3. Event Lifecycle

### 3.1 Creation

A client sends a create request to any node. The receiving node performs steps 1–5 within a **single per-account atomic section**. Implementations must use one of:
- A per-account mutex/lock
- A serialized write path (e.g., single-threaded event loop, actor model)
- A compare-and-swap (CAS) loop that covers the full state check (idempotency + balance + hold reservation)

The critical invariant: the idempotency check, overdraft validation, hold reservation, and event creation for a given `(bucket, account)` are indivisible. No concurrent operation on the same account can interleave between any of these steps. Concurrent credits are safe (they only increase the balance) and MAY bypass the serialization.

Within the atomic section:

1. **Checks idempotency** (if `idempotency_nonce` is present): if an event with the same `(idempotency_nonce, bucket, account, amount)` already exists in the in-memory cache or database, return the existing canonical winning event without creating a new one (§10.3). Release the atomic section and return immediately.

2. **Validates the overdraft guard** (debits only): if the projected available balance after applying BOTH the debit amount and any newly-created hold would fall below the floor (`-max_overdraft` or 0), reject with an error. Release the atomic section and return the error.

3. **Assigns a sequence number**: the next value in this node's monotonic counter. Increment the counter.

4. **Generates a UUID** for the event_id.

5. **Sets hold metadata** (debits only): if the node is configured for balance holds (§11), set `hold_amount` and `hold_expires_at_unix_ms` on the event. The reservation described by these fields is part of the same atomic admission from step 2.

6. **Updates in-memory caches**: balance, available balance, active holds, contiguous head, origin tracking. These updates are the source of truth for subsequent requests — not the database.

7. **Queues the event for persistence** to the node's own database (asynchronous, not blocking the client).

8. **Broadcasts the event to all known peers** (asynchronous). If the client requested quorum acknowledgment (`min_acks > 0`), waits until the specified number of peers confirm receipt or a timeout expires.

9. **Returns the event** to the client along with the updated balance and acknowledgment info.

### 3.2 Replication

When a node receives an event from a peer (via broadcast or sync):

1. **Durable presence claim**: atomically establish durable local presence for `(origin_node_id, origin_epoch, origin_seq)` using an indexed unique key. If local durable storage already contains the key, discard the event as a duplicate. The usual implementation is a unique index on the `events` table (or an equivalent durable claim table) with conflict-skip semantics.

2. **Idempotency conflict check**: if the event has an `idempotency_nonce`, check for an existing event with the same `(idempotency_nonce, bucket, account, amount)`. If a conflict is detected, handle per §10.4.

3. **Update in-memory caches**: balance, available balance, active holds / released holds (if hold metadata or release metadata is present), contiguous head (with gap tracking), origin tracking.

4. **Persist the full event** to this node's own database if the durable presence claim was recorded separately from the append-only event row.

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

### 3.5 Meta Log (`__meta__` bucket)

Administrative operations that affect other buckets are carried as normal events in a dedicated meta bucket named `"__meta__"`. The meta bucket replicates via the exact same per-bucket identity, gossip, and catch-up machinery as any user bucket (§2.1, §4.1, §4.2), with one load-bearing difference: **the meta bucket itself is never deleted**. Every node retains its full meta log forever so a peer that was offline during a meta operation can catch up on it later without any special-case handoff.

**Event types in `__meta__`** (see §2.2):
- `bucket_delete` — hard-delete the bucket named in `event.account`. No other fields are semantically meaningful (`amount=0`, `hold_amount=0`); `note` optionally carries a human-readable reason for the audit trail.

**Apply semantics.** When any node receives a `bucket_delete X` meta event (either as a fresh local emission or via replication):

1. Durably insert the meta event into its own `__meta__` seq line exactly like a normal event.
2. Atomically cascade-delete every row keyed by bucket `X` from storage: events, rolling digests, and the bucket's seq allocator. (Transaction on the postgres backend; equivalent in-memory cleanup for the test backend.)
3. Drop every in-memory entry keyed by `X` from heads, pending seqs, head locks, max-known seqs, digests, event buffer, unpersisted set, per-account state, account-origin-epochs, bucket allocators, and the idempotency cache.
4. Record `X` in `deleted_buckets` (bucket name → delete timestamp). This set is monotonic and never shrinks within a cluster's lifetime.

**Write-path rejection.** Once a bucket is in `deleted_buckets`, every subsequent write to it is rejected at both the local-create path and the replicated-insert path. Reserved bucket names (`__meta__` itself, the `__billing__<user_id>` family) are also rejected at the client-facing RPC. A deleted bucket name is **reserved forever** — there is no undelete, no grace period, no re-use. This is a load-bearing invariant: it lets operators and users reason about a tombstoned name without worrying about time-window races between peers.

**Convergence for late joiners.** A freshly-joining node pulls the full meta log from any peer via the standard `/shardd/range/2` range-fetch, applying each `bucket_delete` as it arrives. Even a node that was offline when the delete happened ends up with the same tombstone set as every other node.

**Authorization.** Clients cannot emit meta events directly. The dashboard exposes:
- `DELETE /api/developer/buckets/{bucket}/purge?confirm={bucket}` — user-facing permanuke, owner-only, typed-confirmation required.
- `DELETE /api/admin/buckets/{bucket}` — admin-only, can delete any bucket regardless of ownership, audit-logged.

Both routes call the gateway's machine-auth-protected `POST /internal/meta/bucket-delete`, which invokes the node-level `NodeRpcRequest::DeleteBucket` to emit the meta event on one node; gossip propagates it to the rest.

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

**Peer selection**: when multiple peers can serve the same range, prefer the peer with the lowest latency. Implementations MAY use libp2p connection metrics or recent request-response RTTs to rank peers; the result should naturally route fetches to the nearest region.

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
8. Start serving protected traffic once readiness criteria are satisfied (§13.2)

Parallel bootstrap dramatically reduces cold-start time for a new node joining a cluster with a large event history. Dedup handles any overlap from conservative range splitting.

A new node NEVER trusts another node's balance values. It always recomputes from the full event log.

## 5. In-Memory State

Each node maintains these caches in memory for fast reads:

| Cache | Key | Value | Purpose |
|-------|-----|-------|---------|
| Balances | (bucket, account) | int64 (atomic) | Balance reads, reporting |
| Available Balances | (bucket, account) | computed | Overdraft checks |
| Active Holds | (bucket, account) | list of {event_id, hold_amount, hold_expires_at_unix_ms} | Available balance computation |
| Released Holds | event_id | bool / set membership | Track hold releases for available balance computation |
| Heads | (origin_id, epoch) | uint64 | Sync protocol, collapsed state |
| Account Origin Epochs | (bucket, account) | set of {(origin_id, epoch)} | Collapsed state computation |
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

Head advancement remains purely in-memory after durable event admission has established local presence.

### 5.2 Balance Updates

Balances are updated atomically (compare-and-swap / reservation for debits, simple add for credits) at the time the event is accepted into the in-memory cache. The database is NOT consulted for balance reads on the hot path, but replicated-event admission MAY consult durable storage for the replay-safe presence check in §3.2.

On startup, balances are rebuilt from the database.

### 5.3 Hold Expiry

Periodically sweep the Active Holds lists and remove expired entries. This is an optimization — expired holds are excluded from `available_balance` by the time check regardless, but cleanup prevents unbounded memory growth. Implementations that cache released holds SHOULD also evict release markers once the underlying hold has expired.

## 6. Persistence Layer

### 6.1 Database Schema

**events** — append-only event log:
- Primary key: `event_id`
- Unique constraint: `(origin_node_id, origin_epoch, origin_seq)` — the dedup key and durable replay-safe presence check
- Columns: all fields from §2.1
- Indexes:
  - `(bucket, account)` for balance aggregation
  - `(created_at_unix_ms)` for time-ordered queries
  - `(void_ref)` WHERE `void_ref IS NOT NULL` — for correction and hold-release lookups
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

All inserts / durable claims use conflict-skip semantics on `(origin_node_id, origin_epoch, origin_seq)`. If a row already exists with the same key, the insert is silently skipped. On replicated admission, only the request that successfully establishes durable local presence for the key may apply the event's in-memory effects; conflicting replays are ignored.

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

### 7.2 Peer Protocols

Peer-to-peer communication does not use HTTP — it runs over libp2p transports (§12). The following protocols are defined on top of libp2p request-response, in addition to the gossipsub topic `shardd/events/v1` for event dissemination (§12.2):

**`/shardd/ack/1`** — Quorum acknowledgment (§12.3)

Request: `AckRequest { event }`
Response: `AckResponse { inserted: bool }`

The receiver calls `state.insert_event(&event)` and reports whether it was newly inserted. Used by the originator to collect `min_acks` synchronous acknowledgments before responding to the client.

**`/shardd/heads/1`** — Per-origin contiguous heads (§4.2)

Request: `HeadsRequest` (unit)
Response: `HeadsResponse { heads: Map<"origin_node_id:epoch", u64> }`

**`/shardd/range/1`** — Event range fetch (§4.2)

Request: `RangeRequest { origin_node_id, origin_epoch, from_seq, to_seq }`
Response: `RangeResponse { events: [Event, ...] }`

The responder serves events first from its in-memory event buffer and falls back to Postgres for older ranges. Ranges are capped (implementation default: 5000 events per request) to keep response sizes bounded.

Peer discovery is handled by Kademlia + Identify (§12.1); there is no join handshake endpoint — a new node dials a bootstrap multiaddr and the DHT does the rest. The node registry is synchronized as part of catch-up sync (§4.2 / §14.3).

Registry inspection and operator actions remain on the HTTP API for client access:

**GET /registry** — Full node registry (all nodes that have ever existed)
**POST /registry/decommission** — Mark a node as decommissioned (operator action)

## 8. Convergence Verification

### 8.1 Head Comparison (Primary, O(origin-epochs))

The primary convergence check. Compare contiguous heads per `(origin_node_id, origin_epoch)` between two nodes. If all heads match for all origin-epoch pairs, the nodes have the same contiguous event prefixes. This is O(number of origin-epoch pairs) — fast regardless of event count.

At 200 nodes with infrequent restarts, the number of origin-epoch pairs stays manageable (a node that has restarted 10 times contributes 10 pairs).

Limitation: heads only prove prefix equality. They don't detect corrupted events within the prefix (same sequence, different payload), and they are not by themselves a replay-safe deduplication authority. Replay safety comes from the durable presence check on `(origin_node_id, origin_epoch, origin_seq)` (§3.2, §6.2).

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

1. **In-memory check**: maintain a map of `(idempotency_nonce, bucket, account, amount) → canonical winning event` for recent events. If the key exists, return that event and balance — do not create a new event.

2. **Database fallback**: if the in-memory cache has been evicted (e.g., after restart), query the events table for matching composite keys. If one or more matches exist, return the canonical winner using the deterministic rule from §10.4.

3. **Response**: a deduplicated request returns the canonical winning event (HTTP 200, not 201).

The in-memory cache may be bounded (e.g., LRU with TTL of 24 hours). After eviction, the database is the backstop.

### 10.4 Cross-Node Conflict Resolution

Two nodes may independently accept events with the same composite idempotency key before sync propagates. This produces two distinct events (different `event_id`, different `origin_node_id`) with the same logical identity.

**Detection**: when a node receives a replicated event, it checks whether a local event with the same `(idempotency_nonce, bucket, account, amount)` already exists. If so, this is an idempotency conflict.

**Winner determination — oldest wins, deterministic**:

1. Lower `created_at_unix_ms` wins.
2. If timestamps are equal: lower `event_id` (lexicographic comparison) wins.

All nodes apply the same rule and agree on the winner.

### 10.5 Correction Emission

Any node that detects an idempotency conflict emits correction events for the loser on the emitting node's own sequence.

When a node detects a conflict (either its own event lost, or it sees an unresolved conflict for a different origin):

1. Create a new `void` event on this node's own sequence:
   - `type`: `void`
   - `bucket`, `account`: same as the voided event
   - `amount`: negation of the voided event's amount (e.g., if the voided event was `-50`, the void event is `+50`)
   - `void_ref`: the `event_id` of the voided (losing) event
   - `idempotency_nonce`: `"void:{loser_event_id}"` — deterministic, derived from the event being voided
   - `hold_amount`: 0
   - `note`: human-readable reason, e.g., `"void: duplicate of event {winner_event_id}"`

2. If the losing event is a debit with an active hold, create a new `hold_release` event on this node's own sequence:
   - `type`: `hold_release`
   - `bucket`, `account`: same as the released debit
   - `amount`: 0
   - `void_ref`: the `event_id` of the debit whose hold is being released
   - `idempotency_nonce`: `"release:{loser_event_id}"` — deterministic, derived from the held event being released
   - `hold_amount`: 0
   - `hold_expires_at_unix_ms`: 0
   - `note`: human-readable reason, e.g., `"release hold: duplicate of event {winner_event_id}"`

3. Before emitting either correction, check whether a local event with the corresponding deterministic idempotency key already exists (in-memory or database). If it does, do not emit the duplicate correction.

4. Broadcast emitted correction events to all peers like any other event.

### 10.6 Multiple Correction Emission

Because broadcast latency between global nodes can be 2–300ms, multiple nodes may independently emit correction events for the same loser before hearing about each other's corrections. This is safe.

- Multiple `void` events for the same loser still conflict with each other. The same deterministic winner rule applies recursively until one canonical correction remains.
- Multiple `hold_release` events for the same loser are harmless. They carry `amount = 0`, and implementations treat a hold as released if at least one matching `hold_release` exists for the referenced event.
- No fixed upper bound on the total number of correction events is guaranteed; it depends on timing and partition behavior. Implementations SHOULD minimize duplicate corrections by checking local cache and database state before emitting.
### 10.7 Balance Model

Balance remains a pure sum over all events:

```
balance(bucket, account) = SUM(amount) for all events WHERE bucket = b AND account = a
```

This includes `standard`, `void`, and `hold_release` events. Void events have negating amounts, so they cancel the duplicate's effect in the sum. `hold_release` events have `amount = 0`, so they affect `available_balance` only — not settled balance.

Example:
```
Event 205 (node-A, seq 205): type=standard, amount=-50, nonce="completion:abc"   → balance: -50
Event 310 (node-B, seq 310): type=standard, amount=-50, nonce="completion:abc"   → balance: -100 (temporary)
Event 524 (node-B, seq 524): type=void, amount=+50, void_ref=event_310_id       → balance: -50  (correct)
```

No derived state. No mutable flags. The log is a set of immutable entries whose sum converges to the correct balance.

### 10.8 Consistency Window

Between conflict detection and correction propagation, the balance is temporarily incorrect (double-charged), and `available_balance` may be temporarily over-reserved until the matching `hold_release` arrives. The duration of this window is bounded by broadcast latency (typically < 100ms within a region) or catch-up sync interval (default 30 seconds).

### 10.9 Operational Notes

- **Nonce generation**: clients SHOULD derive the nonce deterministically from the operation (e.g., `completion:{request_id}`). Random nonces per attempt defeat the purpose.
- **Retry target**: clients SHOULD retry against the same node/region to avoid cross-node conflicts entirely.
- **Events without nonces**: events with `idempotency_nonce = null` bypass idempotency checks. They are never considered duplicates.
- **Correction events in usage history**: `void` and `hold_release` events are visible to users in their usage history, clearly marked with their `type` and a reference to the original event. This provides full auditability.

## 11. Balance Holds

### 11.1 Purpose

When a node accepts a debit, it reserves an additional amount of balance for a configurable duration. This reduces the available balance visible to all nodes, preventing other nodes from spending against balance that is likely to be consumed by further requests arriving at the originating node.

This is a soft distributed lock — it does not require consensus and does not guarantee prevention of overspend, but it significantly reduces the overdraft window in multi-node scenarios.

### 11.2 Hold Lifecycle

1. **Creation**: when a node creates a debit event, it sets `hold_amount` and `hold_expires_at_unix_ms` based on node configuration.

2. **Propagation**: the hold fields travel with the event via broadcast and sync. All nodes that receive the event include the hold in their `available_balance` computation.

3. **Expiry**: holds expire passively. No event is emitted on expiry. Each node independently stops including the hold in `available_balance` once `now_ms >= hold_expires_at_unix_ms`. Since all nodes use the same expiry timestamp from the event, they converge (modulo clock skew).

4. **Early release for corrections only**: a hold MAY be released early by a system-generated `hold_release` event when the underlying debit is corrected (e.g., duplicate resolution). Clients cannot release holds manually.

### 11.3 Available Balance Computation

```
effective_hold(event) =
  event.hold_amount
  IF event.type = standard
     AND event.amount < 0
     AND event.hold_expires_at_unix_ms > now_ms
     AND no hold_release event exists with void_ref = event.event_id
  ELSE 0

active_holds      = SUM(effective_hold(event)) for events WHERE bucket = b AND account = a
available_balance = balance - active_holds
```

If multiple `hold_release` events reference the same held debit, the release is treated as boolean existence, not additive subtraction.

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

When a node receives a replicated correction affecting holds:

1. A debit `standard` event with hold metadata adds a hold to the in-memory Active Holds cache for the relevant `(bucket, account)`.
2. A `hold_release` event marks the referenced hold as released in the Released Holds cache.
3. Subsequent local debit requests for this account see the updated `available_balance` and may be rejected or re-admitted accordingly.

This is the mechanism by which one node's spending activity reduces available balance on other nodes without consensus.

### 11.6 Limitations

- **Clock skew**: nodes with skewed clocks will disagree on whether a hold is active. Nodes MUST run NTP or an equivalent clock synchronization protocol with a maximum drift of 1 second. Hold durations (default 10 minutes) are chosen to be much larger than this bound, making skew negligible relative to hold lifetime. Implementations SHOULD monitor clock drift and alert if it exceeds the bound.
- **Stale holds**: if broadcast is delayed, a node may not know about a peer's holds until catch-up sync. During this window, the node's `available_balance` is higher than it should be.
- **Over-reservation**: aggressive hold multipliers reduce available balance significantly, potentially rejecting legitimate debits. Tune `hold_multiplier` and `hold_duration_ms` based on actual traffic patterns.
- **Correction latency**: after a duplicate debit is detected, `available_balance` remains too low until the matching `hold_release` propagates.

## 12. Broadcast and Membership Layer

The protocol uses **libp2p** as the foundation for peer-to-peer networking, combining peer discovery, event dissemination, and direct queries into a unified stack with built-in encryption.

### 12.1 libp2p Overview

libp2p provides four services in a single stack:

**Peer identity**: each node has a libp2p `PeerId` derived from an Ed25519 keypair, and peers authenticate each other via the Noise protocol (XX handshake) on every connection. **Implementation note**: the current reference implementation generates a fresh keypair on every node start (so the `PeerId` is ephemeral and changes across restarts). Deriving the keypair from the persistent `node_id` for a stable `PeerId` is a planned follow-up.

**Peer discovery**: nodes discover each other via the **Kademlia DHT**. A new node dials one or more bootstrap peers (configured via `bootstrap` multiaddrs); the `Identify` protocol exchanges peer metadata, including the shardd `node_id` embedded in the `agent_version` field as `shardd/{node_id}/{epoch}`. Kademlia is wired as a behaviour and ingests newly-connected peers via `add_address`, but no explicit DHT bootstrap/walk is issued — mesh connectivity currently relies on the bootstrap dial plus libp2p's automatic peer exchange.

**Failure detection**: libp2p tracks connection liveness via the underlying TCP/QUIC transport and gossipsub heartbeats. `ConnectionEstablished` and `ConnectionClosed` swarm events translate directly to `MembershipEvent::Up`/`Down` for the node registry.

**Private mesh (optional)**: if a 32-byte pre-shared key (PSK) is configured, all connections are additionally encrypted with XSalsa20 via `libp2p-pnet`. Nodes without the PSK cannot connect. This provides network-level access control.

### 12.2 Event Dissemination (Gossipsub)

Events are disseminated via **gossipsub**, libp2p's mesh-based publish/subscribe protocol. When a node creates or receives a new event, it publishes to the `shardd/events/v1` topic. Gossipsub's mesh construction ensures the event reaches all subscribers in O(log N) gossip rounds.

Gossipsub properties used by shardd:
- **Mesh-based**: bounded out-degree per peer (default `mesh_n=6`, `mesh_n_high` tightened to `8`), not flooding — scalable to 200+ nodes
- **Message validation**: `ValidationMode::Strict` — messages must be signed by the publisher
- **Message deduplication**: built-in via `message_id`; shardd's event-level dedup (§3.2) provides a second layer
- **Peer scoring (gossipsub v1.1)**: available but **not currently enabled** — default `PeerScoreParams` have no per-topic weights, so they cannot rebalance hub load without application-specific tuning. Deferred until we have bench data.

Events are serialized with JSON (matching the rest of the API) and published directly. Gossipsub handles backpressure internally via its mesh construction.

### 12.3 Quorum Acknowledgments

When a client requests `min_acks > 0`, the node uses libp2p's **request-response protocol** on `/shardd/ack/1` to collect synchronous acknowledgments:

1. Select up to `min_acks` connected peers
2. Send `AckRequest { event }` to each via request-response
3. Collect `AckResponse { inserted: bool }` responses
4. Return when `received >= min_acks` OR all outstanding requests complete OR timeout expires

The receiving peer calls `state.insert_event()` and responds with `inserted: bool`. This provides reliable, low-latency quorum without depending on gossipsub's eventual consistency.

### 12.4 Persistence Notifications

After a batch write to the database succeeds, the node would ideally disseminate a "persisted" notification listing the `(origin_node_id, origin_epoch, origin_seq)` keys that were written. In the current implementation this is best-effort / not wired — the information is primarily used by `GET /persistence` for local introspection.

### 12.5 Sync Protocol (libp2p request-response)

Catch-up sync (§4.2) runs over libp2p **request-response** protocols, keeping peer sync on the same transport as event dissemination and quorum acks:

- **`/shardd/heads/1`** — `HeadsRequest` → `HeadsResponse { heads: Map<"origin:epoch", u64> }`. Returns the querying peer's contiguous heads per `(origin_node_id, origin_epoch)`.
- **`/shardd/range/1`** — `RangeRequest { origin_node_id, origin_epoch, from_seq, to_seq }` → `RangeResponse { events }`. Returns the events in the requested sequence range, served first from the in-memory event buffer and then from Postgres for older ranges.

A periodic catch-up loop on each node picks connected peers, queries their heads, compares against local heads, and fetches missing ranges (capped to 5000 events per request) from whichever peer has them. Dedup at the state layer handles any overlap.

Peers are discovered via Kademlia DHT + Identify (§12.1); no separate address registry is needed for sync.

### 12.6 Implementation

In Rust, use **rust-libp2p** v0.56. The `LibP2pBroadcaster` in `libs/broadcast/src/libp2p.rs` wraps:
- **TCP transport** with Noise encryption and Yamux multiplexing
- **gossipsub** (`libp2p-gossipsub`) for the `shardd/events/v1` topic
- **request-response** (`libp2p-request-response` with JSON codec) for `/shardd/ack/1`
- **Kademlia DHT** (`libp2p-kad`) for peer discovery
- **Identify** (`libp2p-identify`) for peer metadata exchange
- **pnet** (`libp2p-pnet`) for optional PSK encryption

The Swarm runs in a dedicated tokio task. Commands from the Broadcaster trait methods arrive via an mpsc channel; swarm events (connections, gossipsub messages, ack responses) are forwarded out via unbounded channels for the main.rs bridge tasks (registry updates, event application).

### 12.7 Configuration

| Parameter | Default | Description |
|-----------|---------|-------------|
| `libp2p_port` | `9000` | TCP port for libp2p transport |
| `psk_file` | unset | Path to 32-byte PSK file for private mesh encryption |
| `bootstrap` | `[]` | Bootstrap peer multiaddrs (e.g., `/ip4/1.2.3.4/tcp/9000`) |
| `event_worker_count` | 4 | Parallel workers decoding + applying inbound gossipsub events (recv is serialized; decode + `state.insert_event` run in parallel) |

Gossipsub tuning (non-default values):
- `heartbeat_interval = 700ms` (default 1s) — slightly snappier grafts/prunes.
- `mesh_n_high = 8` (default 12) — caps per-peer mesh degree in small-to-medium clusters.

All other gossipsub parameters (`mesh_n`, `mesh_n_low`, `mesh_outbound_min`, `gossip_lazy`, etc.) use libp2p defaults. Peer scoring (§12.2) is wired but not enabled.

## 13. Node Lifecycle

### 13.1 Startup (Existing Node)

1. Connect to database, run schema migrations
2. Load or create node identity from database (no per-node epoch / next_seq — those are per-bucket now)
3. Mark every `bucket_seq_allocator` row owned by this node `needs_bump = TRUE` in a single atomic UPDATE. This is the only "epoch bump" the node does unconditionally at startup — the actual per-bucket bump is deferred to the first write to that bucket.
4. Load node registry from database
5. Rebuild in-memory caches from database:
   - Balances: aggregate `SUM(amount) GROUP BY bucket, account`
   - Active holds: load unexpired holds from events
   - Released holds: load referenced event IDs from `hold_release` events
   - Heads: compute contiguous heads from event sequences per `(bucket, origin_node_id, origin_epoch)` for ALL origins in registry
   - Origin-account mapping: `DISTINCT (bucket, origin_node_id, origin_epoch, account)`
   - Idempotency cache: recent events with non-null nonces
   - Bucket allocators: load `bucket_seq_allocator` rows for this node — inherit `needs_bump` from the DB flag just set in step 3
6. Dial bootstrap peers via libp2p — Kademlia populates the routing table, Identify supplies peer metadata, gossipsub forms the event mesh
7. Apply libp2p connection events (`ConnectionEstablished` / `ConnectionClosed` + `Identify::Received`) to the node registry
8. Start background tasks (batch writer, orphan detector, catch-up sync)
9. Enter a warming state. Accept protected client traffic only after readiness criteria are satisfied (§13.2).

**Lazy per-bucket epoch bump**: when the first write of this process
lifetime arrives for bucket `B`:

1. If `needs_bump` is set in memory: transactionally `UPDATE
   bucket_seq_allocator SET current_epoch = current_epoch + 1, next_seq
   = 1, needs_bump = FALSE WHERE bucket = $B AND node_id = self.node_id`.
   The post-bump `current_epoch` is the new durable epoch for this
   bucket.
2. If the row does not exist (first-ever write to `B` from this node):
   insert one with `current_epoch = 1, next_seq = 1, needs_bump = FALSE`.
3. Allocate the new event's `origin_seq` by `next_seq.fetch_add(1)` and
   use it alongside the bucket's `current_epoch`.

Buckets the node never writes to in this process lifetime **never bump**.
That eliminates the v1.8 failure mode where every restart created a new
`(origin, epoch)` tuple that every peer had to track forever, even for
buckets that had no new events.

### 13.2 New Node Joining

Same as 13.1, plus after step 8:
- Receive full node registry from bootstrap peer via catch-up sync (§4.2)
- Merge received registry into local database (covers historical origins libp2p has not yet observed live)
- Run trustless bootstrap: pull ALL events from ALL origins in the registry, recompute state
- **Readiness gate**: do not mark as healthy or accept protected client traffic until local heads are within a configurable threshold of peers' heads. Protected traffic includes, at minimum, debits and any request with a non-null `idempotency_nonce`. This prevents a cold node from approving balance-sensitive or nonce-sensitive writes against stale state.

### 13.3 Graceful Shutdown

1. Stop accepting new requests
2. Flush batch writer (write all buffered events to database)
3. Close libp2p connections (gossipsub prune + TCP FIN); peers observe `ConnectionClosed` and transition the registry entry to `suspect`/`unreachable`
4. Shut down

### 13.4 Crash Recovery

On restart after a crash:
- Events in the batch writer buffer (not yet flushed) are lost from THIS node's database.
- Those events may already have been broadcast to peers before the crash (broadcast happens immediately after enqueueing, before the next flush).
- Peers' orphan detectors persist those events to their databases.
- The lost events belong to the prior per-bucket epoch. They will be synced back during catch-up.

**Per-bucket epoch recovery**: each bucket has its own lazy-bump epoch
(§13.1). On restart, every existing `bucket_seq_allocator` row is
flagged `needs_bump = TRUE`. The first write to a bucket after startup
atomically bumps that bucket's `current_epoch` and resets its
`next_seq = 1`. There is no risk of sequence reuse — the new
`(bucket, origin_node_id, origin_epoch)` triple is a distinct sequence
space. Events from the prior epoch (including any that were broadcast
but not flushed locally) are recovered from peers during normal
catch-up sync for that bucket, independently from other buckets.

**Startup sequence**:
1. Read own DB: no node-wide epoch exists. Just persist node identity.
2. Flag every `bucket_seq_allocator` row for this node `needs_bump = TRUE`.
3. Dial bootstrap peers via libp2p, begin catch-up sync (recovers unflushed events from prior epoch, per bucket).
4. Resume network participation immediately, but keep protected client writes disabled until catch-up / readiness completes. Implementations MAY accept non-idempotent credits earlier because they only increase balance and do not depend on prior nonce state.

This is the primary advantage of the per-bucket epoch mechanism: a
crashed node can resume replication immediately without waiting for
peer sync, because the bumped epoch-per-bucket guarantees no sequence
collision *even on buckets that were mid-write at the crash*. It does
NOT make debit admission or nonce-sensitive write admission safe on its
own — those still depend on recovering prior-epoch balance, hold, and
idempotency state, so the readiness gate (§13.2) applies to protected
writes as well as accuracy-sensitive reads.

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

### 14.2 libp2p-Driven Membership

libp2p connection events (§12.1) are the primary source of membership information. The node registry is the persistent store that records the live view.

**libp2p → Registry integration** (current implementation):

| libp2p Event | Registry Action |
|--------------|----------------|
| `ConnectionEstablished` + `Identify::Received` (carries `node_id` in `agent_version`) | Upsert registry entry with `status = active`, set `addr` from the first `Identify::listen_addrs` entry |
| `ConnectionClosed` | Update registry entry to `status = unreachable` |

Only these two events drive registry updates today. Gossipsub and request-response traffic do not refresh `last_seen_at_unix_ms`, and there is no suspicion timer — closed connections transition directly to `unreachable` (see §14.5 note).

### 14.3 Registry Propagation (Safety Net)

Live membership comes from libp2p connection events. The permanent registry converges primarily by local observation: every peer this node has ever talked to via libp2p leaves an entry. Historical origins that predate a node's join (e.g., peers that have decommissioned) can only be learned from the event stream itself:

**Event-driven discovery**: when a node pulls an event range via `/shardd/range/1` (§4.2) and observes an `origin_node_id` not yet in its registry, it can insert a skeleton entry for that origin. This covers origins that existed before this node joined and are no longer reachable live.

**CRDT merge semantics** (defined but not currently exercised over the wire): when registry entries from two sources need to be merged (e.g., a future registry-exchange RPC, or reconciling local state with decisions made by an operator), use a field-specific join:

```
merge(local, remote) → result:
  result.node_id          = local.node_id  (same key)
  result.first_seen_at_unix_ms = MIN(local.first_seen_at_unix_ms, remote.first_seen_at_unix_ms)
  result.last_seen_at_unix_ms  = MAX(local.last_seen_at_unix_ms, remote.last_seen_at_unix_ms)
  result.addr             = (whichever entry has the later last_seen_at_unix_ms).addr

  # Status merge — decommissioned is a monotonic tombstone (CRDT join-semilattice):
  if local.status == "decommissioned" OR remote.status == "decommissioned":
    result.status = "decommissioned"   # once decommissioned, always decommissioned
  else:
    result.status = (whichever entry has the later last_seen_at_unix_ms).status
```

The `decommissioned` status is a tombstone — once set by an operator, it is never overridden by any merge, regardless of timestamps. This prevents a stale view from resurrecting a retired node. All other fields follow "latest wins" semantics.

This merge function is commutative, associative, and idempotent (a CRDT join), so registries converge regardless of merge order or duplication. **Implementation note**: the current `/shardd/heads/1` and `/shardd/range/1` protocols do NOT carry registry payloads. A dedicated registry-exchange RPC is a planned follow-up.

**Consistency check**: a node can verify registry completeness by comparing its set of known `origin_node_id` values from the events table against its registry. Any origin present in events but missing from the registry indicates a gap that should trigger a skeleton insert.

### 14.4 Active Peer Set

The **active peer set** is derived directly from libp2p's current connected-peer set. This replaces manual peer tracking.

At scale (200+ nodes), not every live node needs a dedicated mesh slot. Gossipsub's mesh is bounded by its own parameters (`mesh_n`, `mesh_n_high`, see §12.6); nodes outside a given peer's mesh still receive events via gossipsub's IHAVE/IWANT lazy push and Kademlia-assisted discovery. Implementations MAY additionally cap direct connections via libp2p's connection limits.

A node NOT in any local mesh still has its events synced — during catch-up sync, the node fetches events for ALL origins in the registry, not just currently-meshed peers.

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

- `active → unreachable`: libp2p `ConnectionClosed` event (current implementation transitions directly; no intermediate `suspect` state)
- `unreachable → active`: libp2p reconnects successfully (new `ConnectionEstablished` + `Identify::Received`)
- `active/unreachable → decommissioned`: manual operator action only. A decommissioned node is never contacted again but its events remain in the log and registry.

**Implementation note**: the `suspect` status is defined in the schema and appears in the merge table above, but no code path produces it today. Adding a suspicion window (e.g., 30s after `ConnectionClosed` before declaring `unreachable`) is a planned follow-up; the state machine diagram reflects that intended behaviour.

Unreachable nodes remain in the registry permanently. Their events are still fetched during catch-up sync from any peer that has them.

### 14.6 Self-Exclusion

A node MUST NOT add itself to its own active peer set. The node's own `node_id` is excluded when deriving the active set.

## 15. Configuration

| Parameter | Default | Description |
|-----------|---------|-------------|
| `libp2p_port` | `9000` | libp2p TCP port |
| `psk_file` | unset | 32-byte PSK file for libp2p private mesh encryption |
| `database_url` | — | Database connection string |
| `bootstrap` | [] | libp2p bootstrap peer multiaddrs (e.g., `/ip4/.../tcp/...`) |
| `event_worker_count` | 4 | Parallel workers decoding + applying inbound gossipsub events |
| `batch_flush_interval_ms` | 100 | Batch writer flush interval |
| `batch_flush_size` | 1000 | Batch writer flush size threshold |
| `matview_refresh_ms` | 5000 | Balance summary refresh interval |
| `orphan_check_interval_ms` | 500 | Orphan detector scan interval |
| `orphan_age_ms` | 500 | Minimum age before an event is considered orphaned |
| `hold_multiplier` | 10 | Hold amount = abs(charge) × multiplier |
| `hold_duration_ms` | 600000 | Hold duration (10 minutes) |

libp2p / gossipsub implementation parameters are in §12.6.

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
| **Immutability** | Events are never modified or deleted. Corrections are made by appending `void` and `hold_release` events. |
| **Replay-safe local dedup** | Once a node has durably claimed `(origin_node_id, origin_epoch, origin_seq)`, it will not re-apply that event after restart or replay. |
| **Commutative balance** | Balance = SUM(amount) over all events. Order of event arrival does not affect final balance. |

This system provides AP (availability + partition tolerance) from the CAP theorem, sacrificing strong consistency for eventual consistency.

## 17. Open Issues and Future Work

- **Strict overdraft enforcement**: requires consensus (e.g., Raft) or routing all debits for an account to a single node. Documented as a known limitation (§9.2).
- **Rolling prefix digests**: incremental convergence verification (§8.3) is recommended but not required by this protocol version.
- **Event signing**: no cryptographic signatures on events. In an untrusted environment, a malicious node can forge events with any `origin_node_id`. Adding ed25519 signatures per event would prevent this.
- **Rate limiting**: no built-in mechanism. Implementations should add per-client or per-account rate limits at the API layer.
- **Hold tuning**: optimal `hold_multiplier` and `hold_duration_ms` values depend on traffic patterns. Future versions may support adaptive holds based on observed request rates.
- **Log compaction**: for deployments with very high event volume, a snapshotting and truncation mechanism may be needed. Not addressed in this version.
- **Epoch proliferation**: frequent restarts produce many origin-epoch pairs, increasing the size of head comparison (§8.1) and sync plan computation. At 200 nodes with 100 restarts each, the head table has 20,000 entries. Manageable, but implementations may want to coalesce finalized epochs (where the head is known to be complete and no new events will arrive) into a single digest for comparison efficiency.
