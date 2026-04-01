# shardd Architecture

## Overview

shardd is a distributed, multi-writer, eventually-consistent append-only ledger. Each node independently accepts credit/debit events, stores them in its own PostgreSQL, and replicates to peers via HTTP broadcast. Postgres is NOT on the hot path — events propagate instantly via broadcast and are persisted asynchronously.

## Node Architecture

```
┌─────────────────────────────────────── Node ──────────────────────────────────────┐
│                                                                                    │
│   Client Request                                                                   │
│        │                                                                           │
│        ▼                                                                           │
│   ┌─────────┐                                                                      │
│   │  Axum   │  POST /events                                                       │
│   │ Router  │──────────────────────────────────────────┐                           │
│   │ (HTTP)  │  POST /events/replicate                  │                           │
│   └────┬────┘──────────────────────┐                   │                           │
│        │                           │                   │                           │
│        ▼                           ▼                   ▼                           │
│   ┌─────────────────────────── SharedState ─────────────────────────────────┐      │
│   │                                                                         │      │
│   │  DashMap Caches (lock-free concurrent reads):                          │      │
│   │  ┌──────────────┐  ┌──────────────┐  ┌────────────────────────┐        │      │
│   │  │   accounts   │  │    heads     │  │   account_origins      │        │      │
│   │  │ {bucket,acct}│  │ origin_id →  │  │ {bucket,acct} →        │        │      │
│   │  │ → balance    │  │ contiguous   │  │ {origin_ids}           │        │      │
│   │  │  (AtomicI64) │  │ head (u64)   │  │                        │        │      │
│   │  └──────────────┘  └──────────────┘  └────────────────────────┘        │      │
│   │                                                                         │      │
│   │  Persistence Tracking:                                                  │      │
│   │  ┌──────────────┐  ┌──────────────┐  ┌────────────────────────┐        │      │
│   │  │ event_buffer │  │ unpersisted  │  │   pending_seqs         │        │      │
│   │  │ {origin,seq} │  │ {origin,seq} │  │ origin_id →            │        │      │
│   │  │ → Event      │  │ → created_ms │  │ BTreeSet<seq>          │        │      │
│   │  └──────────────┘  └──────────────┘  └────────────────────────┘        │      │
│   │                                                                         │      │
│   │  Atomics: next_seq, event_count, total_balance                         │      │
│   └─────────────────────────────────────────────────────────────────────────┘      │
│        │                          │                           │                    │
│        │ mpsc channel             │ Broadcaster trait          │ reads              │
│        ▼                          ▼                           ▼                    │
│   ┌──────────┐           ┌──────────────┐            ┌──────────────┐             │
│   │  Batch   │           │ Broadcaster  │            │   Orphan     │             │
│   │  Writer  │           │ (HTTP/       │            │  Detector    │             │
│   │          │           │  Gossip/     │            │              │             │
│   │ flush    │           │  InMemory)   │            │ scan every   │             │
│   │ every    │           │              │            │ 500ms        │             │
│   │ 100ms    │           │ push to ALL  │            │              │             │
│   └────┬─────┘           │ peers        │            └──────┬───────┘             │
│        │                 └──────┬───────┘                   │                     │
│        │ bulk INSERT            │                           │ bulk INSERT          │
│        ▼                        ▼                           ▼                     │
│   ┌──────────┐           ┌──────────────┐            ┌──────────┐               │
│   │ Postgres │           │ Other Nodes  │            │ Postgres │               │
│   │ (own)    │           │ via HTTP     │            │ (own)    │               │
│   └──────────┘           └──────────────┘            └──────────┘               │
│                                                                                    │
│   Background: Catch-up Sync (30s safety net)                                      │
│   Background: Matview refresh (5s)                                                │
└────────────────────────────────────────────────────────────────────────────────────┘
```

## Data Flow: Local Event Creation

```
POST /events {bucket, account, amount, min_acks: 2}
  │
  ├── 1. Overdraft check (atomic CAS on balance)
  │      └── if rejected → 422 Insufficient Funds
  │
  ├── 2. Allocate sequence (next_seq.fetch_add, Relaxed)
  │
  ├── 3. Update in-memory caches (all lock-free):
  │      ├── accounts: balance + event_count atomics
  │      ├── heads: advance contiguous head (drain pending_seqs)
  │      ├── account_origins, max_known_seqs
  │      ├── event_buffer: store full event for orphan recovery
  │      └── unpersisted: track as not-yet-in-Postgres
  │
  ├── 4. Queue for async persistence (mpsc channel → BatchWriter)
  │      └── BatchWriter bulk INSERTs every 100ms, never blocks client
  │
  ├── 5. Broadcast to peers (HTTP POST /events/replicate)
  │      ├── min_acks=0: fire-and-forget (tokio::spawn, instant return)
  │      └── min_acks=2: wait for 2 HTTP 200 responses (or timeout)
  │
  └── 6. Return 201 {event, balance, acks: {received: 2, requested: 2}}
```

**Postgres is never on the hot path.** The client gets a response as soon as in-memory state is updated and (optionally) peers have acknowledged.

## Data Flow: Crash Recovery

```
Normal: Node A creates event → broadcasts to B,C → BatchWriter flushes to A's PG

Node A crashes before BatchWriter flush:
  │
  ├── Event is in B and C's event_buffer (received via broadcast)
  │
  ├── 500ms later: B's OrphanDetector finds it unpersisted
  │   └── bulk INSERT to B's Postgres (ON CONFLICT = safe)
  │       └── broadcasts "persisted" → C marks it done too
  │
  └── Node A restarts:
      └── Runs bootstrap_from_peers()
          └── Pulls event from B via GET /heads + POST /events/range
              └── Writes to own Postgres → state rebuilt
```

**Zero events lost** as long as at least one peer received the broadcast before the crash.

## Data Flow: New Node Bootstrap (Trustless)

```
New node starts with empty Postgres:
  │
  ├── 1. Connect to cluster via --bootstrap peers
  ├── 2. POST /join to each peer → get peer list + heads
  ├── 3. For each origin, for each peer:
  │      └── POST /events/range in 10K chunks → get ALL events
  ├── 4. insert_events_batch() → DashMap caches + BatchWriter
  ├── 5. Recompute balances from events (trustless, not from peers)
  └── 6. Start serving
```

New nodes never trust another node's balance view. They pull all events and recompute.

## Storage Model

Each node has its own PostgreSQL instance. Events replicate via broadcast; each node writes ALL events (own + replicated) to its own PG.

```
Node A (PG-A) ◄──broadcast──► Node B (PG-B) ◄──broadcast──► Node C (PG-C)
```

- **Lose any node**: Others keep running. Lost events already replicated.
- **Add any node**: Bootstraps from peers. No shared infrastructure.
- **Restart**: Reads own PG + catches up from peers.

### Postgres Schema

```sql
events          — append-only event log
  event_id      TEXT PRIMARY KEY
  origin_node_id, origin_seq  — UNIQUE (dedup key)
  bucket, account, amount     — ledger data
  note, inserted_at           — metadata

node_meta       — node identity + sequence counter
peers           — known peer addresses
balance_summary — materialized view (SUM(amount) GROUP BY bucket, account)
```

### What's in Memory vs Postgres

| Data | In Memory (DashMap) | In Postgres |
|------|-------------------|-------------|
| Balances | ✓ (hot reads) | ✓ (matview, startup) |
| Heads | ✓ (sync protocol) | Computed from events |
| Events | ✓ (recent, in event_buffer) | ✓ (all, append-only) |
| Unpersisted tracking | ✓ | — |
| Pending out-of-order seqs | ✓ | — |

## Broadcast Abstraction

The `Broadcaster` trait hides transport details:

| Implementation | Transport | Use Case |
|---------------|-----------|----------|
| `HttpBroadcaster` | HTTP POST to known peers | Simple deployments |
| `GossipBroadcaster` | foca SWIM protocol (UDP) | Large clusters, auto-discovery |
| `InMemoryBroadcaster` | tokio channels | Unit tests |

Selected via `--broadcast-mode http|gossip`.

### Per-Request Quorum Acks

```json
POST /events {
  "bucket": "default", "account": "alice", "amount": -500,
  "min_acks": 2, "ack_timeout_ms": 300
}
```

- `min_acks=0`: fire-and-forget (fastest)
- `min_acks=2`: wait for 2 peers to HTTP 200 before responding
- On timeout: event is still created, response indicates partial acks

## Background Tasks

| Task | Interval | Purpose |
|------|----------|---------|
| **BatchWriter** | 100ms / 1000 events | Bulk INSERT to own Postgres |
| **OrphanDetector** | 500ms | Persist events from crashed peers |
| **Catch-up Sync** | 30s | Pull missing events from peers (safety net) |
| **Matview Refresh** | 5s | Refresh balance_summary view |

## API Endpoints

| Method | Path | Purpose |
|--------|------|---------|
| GET | /health | Node status |
| GET | /state | Full state (heads, checksum, peers) |
| GET | /events | All events (from Postgres) |
| GET | /heads | Contiguous heads per origin |
| GET | /balances | All account balances |
| GET | /collapsed | Balance + sync status per account |
| GET | /collapsed/:bucket/:account | Single account |
| GET | /persistence | Unpersisted event count/age |
| GET | /debug/origin/:id | Gap analysis for an origin |
| POST | /events | Create event (with min_acks) |
| POST | /events/replicate | Accept replicated event |
| POST | /events/range | Fetch events by origin+seq range |
| POST | /join | Cluster join handshake |
| POST | /peers/add | Add peer manually |
| GET | /peers | List peers |

## Collapsed State

Each account has a completeness status based on whether all contributing origins have contiguous sequences:

```json
GET /collapsed
{
  "default:alice": {
    "balance": 150,
    "status": "locally_confirmed",
    "contributing_origins": {
      "node-A": {"head": 500, "max_known": 500},
      "node-B": {"head": 300, "max_known": 300}
    }
  },
  "default:bob": {
    "balance": 70,
    "status": "provisional",
    "contributing_origins": {
      "node-B": {"head": 300, "max_known": 307}
    }
  }
}
```

- **locally_confirmed**: all origins have gapless heads — balance is final
- **provisional**: gaps exist — balance may change when missing events arrive

## Checksum

Canonical format shared with elixir_ledger:

```
{origin_node_id}:{origin_seq}:{event_id}:{bucket}:{account}:{amount}\n
```

SHA-256, hex-encoded, ordered by `(origin ASC, seq ASC)`. Excludes `note` (cosmetic field).
