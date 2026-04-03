# PLAN-0001: shardd v2 Full Rewrite from Protocol Spec

**Created**: 2026-04-03
**Status**: Ready for implementation

## Summary

Complete rewrite of shardd on a `v2` branch. All `.rs` source files deleted; workspace skeleton (Cargo.toml, deps) and `docs/protocol.md` v1.7 retained. The protocol spec is the sole source of truth. Produces a production-ready distributed ledger node with SWIM gossip, balance holds, idempotency with cross-node void resolution, epoch-based crash recovery, and per-request quorum acks.

## Requirements

### Event Model (§2)
- 14-field Event struct: event_id, origin_node_id, origin_epoch, origin_seq, created_at_unix_ms, type (standard/void/hold_release), bucket, account, amount, note, idempotency_nonce, void_ref, hold_amount, hold_expires_at_unix_ms
- Dedup key: `(origin_node_id, origin_epoch, origin_seq)`

### Event Lifecycle (§3)
- Per-account atomic section: idempotency check + overdraft guard + hold reservation + event creation are indivisible
- Async persistence via BatchWriter (100ms / 1000 events)
- OrphanDetector for crash recovery (500ms scan)
- Durable presence claim on replication

### Sync (§4)
- Primary: broadcast (gossip piggyback + HTTP fallback)
- Catch-up: 30s safety net with parallel fetching
- Trustless bootstrap: pull ALL events, recompute balances

### In-Memory State (§5)
- 11 caches: Balances, Available Balances, Active Holds, Released Holds, Heads, Account Origin Epochs, Max Known Seqs, Event Buffer, Unpersisted, Pending Seqs, Idempotency Cache
- Per-account `Mutex<AccountState>` for the atomic section

### Persistence (§6)
- Tables: events (14 cols + 4 indexes), node_meta (with current_epoch), node_registry (permanent CRDT), balance_summary (optional matview)
- Epoch increment: single atomic `UPDATE ... RETURNING`

### API (§7)
- Client: POST /events (with idempotency_nonce, min_acks, ack_timeout_ms), GET /health, /state, /events (paginated), /heads, /balances (with available_balance), /collapsed, /persistence, /debug/origin/:id
- Peer: POST /events/replicate, /events/range (with origin_epoch), /join (returns registry+heads), GET /registry, POST /registry/decommission

### Overdraft Guard (§9)
- Checks `available_balance + amount >= -max_overdraft`
- `available_balance = balance - active_holds`

### Idempotency (§10)
- Composite key: (nonce, bucket, account, amount)
- Local enforcement in per-account atomic section
- Cross-node: oldest wins → void emission + hold_release for losers

### Balance Holds (§11)
- hold_amount + hold_expires_at_unix_ms on debit events
- NTP required (max 1s drift)
- Hold_release events for voided held debits

### SWIM/Gossip (§12)
- foca for SWIM: probe cycles, indirect probes, suspicion, dead transitions
- Gossip buffer: 10K capacity, drop-oldest, retransmit fanout×3
- HTTP fallback at 80% buffer fill
- Quorum acks via direct HTTP POST

### Node Lifecycle (§13)
- Startup: increment epoch, rebuild caches, join SWIM, readiness gate
- Graceful shutdown: flush, leave SWIM
- Crash recovery: new epoch, catch-up sync

### Registry (§14)
- Permanent node_registry, never deleted
- CRDT merge: decommissioned is monotonic tombstone
- SWIM → registry integration

## Scope

### In Scope
- Node binary, types lib, storage lib, broadcast lib, CLI, benchmarks
- Dockerfile, docker-compose (per-node Postgres)
- Protocol-spec tests
- Production readiness: tracing, graceful shutdown, health/readiness, error handling

### Out of Scope
- Dashboard (Dioxus WASM)
- elixir_ledger changes
- Rolling prefix digests (§8.3)
- Log compaction (§17)

## Anti-Goals
- No deviating from protocol spec
- No carrying over v1 patterns that conflict with the spec
- No over-engineering beyond spec + production basics

## Non-Negotiables
1. Dedup key: `(origin_node_id, origin_epoch, origin_seq)` enforced at DB + memory
2. Per-account atomic section: idempotency + overdraft + hold = indivisible
3. Balance = SUM(amount) — no derived mutable state

## Design

### Architecture
```
shardd/
├── libs/
│   ├── types/         Event, OriginKey, EpochKey, all request/response types
│   ├── storage/       StorageBackend trait, PostgresStorage, InMemoryStorage, migrations
│   └── broadcast/     Broadcaster trait, HttpBroadcaster, GossipBroadcaster (foca), InMemoryBroadcaster
├── apps/
│   ├── node/          Main binary: state machine, API, background tasks
│   ├── cli/           HTTP client for all endpoints
│   └── bench/         Load testing + multi-node simulation
├── Dockerfile
└── docker-compose.yml
```

### Concurrency Model
- `DashMap<BalanceKey, Arc<Mutex<AccountState>>>` for per-account serialization
- `AccountState` holds: balance (i64), holds (Vec<Hold>), released holds (HashSet<String>), idempotency cache entries
- Credits MAY bypass the mutex (they only increase balance)
- Entry-level lock on event_buffer for replication dedup safety

### Background Task Supervision
- `JoinSet`-based supervision in main loop
- All tasks (BatchWriter, OrphanDetector, catch-up sync, SWIM driver) restart on panic
- Health monitoring: if unpersisted events exceed threshold, mark unhealthy

### Node State Machine
- `enum NodePhase { Warming, Ready, ShuttingDown }`
- Middleware checks phase on every request
- Warming: reject protected writes (debits, idempotent requests), allow reads + replication
- ShuttingDown: reject all writes, return 503

### Durability
- `min_acks=0` has NO durability guarantee (documented)
- `min_acks >= 1` guarantees at least one peer has the event
- Events may be lost if node crashes before broadcast completes AND before BatchWriter flushes

## Key Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Rewrite vs upgrade | Full rewrite | User decision — clean codebase |
| Per-account lock | `Mutex<AccountState>` in DashMap | CAS on AtomicI64 insufficient for idempotency+holds |
| min_acks=0 durability | Document as no guarantee | Adding WAL or mandatory sync write would defeat the async architecture |
| foca integration | Full SWIM, built as standalone module | Protocol mandates it; test in isolation first |
| Nullable acks in 200 | Always return AckInfo::fire_and_forget() | Avoids forcing null-checks on every consumer |
| GET /events | Paginated with cursor | Unbounded response is a DoS vector |

## Risks & Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| foca API mismatch | M | H | Build FocaDriver as standalone module, test with simulated cluster before integration |
| Void cascade storm (O(N) corrections) | L | M | Metrics/alerting on void rates; recursion depth limit |
| Epoch proliferation at scale | L | L | Track in monitoring; implement epoch coalescing later |
| Clock skew breaking hold semantics | L | M | NTP requirement documented; validate hold_expires_at_unix_ms on replication |
| Full rewrite takes longer than expected | H | H | Phase delivery; ship core (Phase 1-2) before SWIM (Phase 3) |

## Implementation Phases

### Phase 1: Foundation (types, storage, core state)
- Event struct with all 14 fields, `OriginKey`/`EpochKey` types
- DB migrations: events (14 cols + indexes), node_meta (with epoch), node_registry, balance_summary matview
- PostgresStorage + InMemoryStorage implementing StorageBackend trait
- SharedState with per-account `Mutex<AccountState>`, all 11 caches
- BatchWriter + OrphanDetector
- Epoch increment on startup
- Basic HTTP API (POST /events, GET /health, /state, /events, /heads, /balances)
- Tests: event creation, overdraft guard, dedup, head advancement, epoch on restart

### Phase 2: Idempotency, Voids, Holds
- Idempotency cache + local enforcement in atomic section
- Cross-node conflict detection + void emission
- hold_release emission for voided held debits
- Available balance computation with hold expiry
- Overdraft guard against available_balance
- Hold expiry sweeper
- Tests: same-node dedup, cross-node conflict, void cascade, hold interaction

### Phase 3: SWIM/Gossip + Registry
- Node registry: DB table, CRDT merge, API endpoints
- foca integration: FocaDriver with UDP transport, timers, BroadcastHandler
- Gossip buffer management (capacity, drop, retransmit, HTTP fallback)
- SWIM → registry integration (membership events → status updates)
- Broadcaster trait: HttpBroadcaster + GossipBroadcaster, flag-selectable
- InMemoryBroadcaster for tests
- Tests: registry CRDT merge, SWIM membership, gossip delivery

### Phase 4: Sync + Bootstrap + Readiness
- Catch-up sync with parallel fetching and registry exchange
- Trustless bootstrap: pull ALL events from ALL origins in registry
- Readiness gate (NodePhase enum + middleware)
- Graceful shutdown (flush + SWIM leave)
- JoinSet-based task supervision
- Tests: bootstrap from peers, crash recovery with epoch, readiness gate

### Phase 5: CLI, Bench, Docker, Polish
- CLI: all endpoints, --idempotency-nonce, --min-acks, collapsed, registry, decommission
- Benchmarks: single-node ab, multi-node Docker, convergence test
- Dockerfile + docker-compose (per-node Postgres)
- Tracing/logging throughout
- Protocol compliance test suite
- Documentation updates

## Implementation Notes

- Void + hold_release must be emitted atomically (sequential seqs, single code path)
- Batch replication triggers conflict detection per-event, not batch-level
- Startup cache rebuild runs in single DB transaction (REPEATABLE READ)
- SWIM parameters for global: probe_timeout 1000ms+, suspicion 15s+
- Validate hold_expires_at_unix_ms on replication: cap at max 24h
- 422 error: rename projected_balance to projected_available_balance
- GET /health: add ready bool + current_epoch
- GET /balances: include available_balance + active_hold_total per account

## Acceptance Criteria

- [ ] All 14 Event fields present and persisted
- [ ] Per-account atomic section covers idempotency + overdraft + holds
- [ ] Epoch increments on every startup; no sequence reuse across restarts
- [ ] Cross-node idempotency conflict produces void + hold_release; balance converges
- [ ] SWIM gossip delivers events; HTTP fallback activates at buffer saturation
- [ ] Node registry is permanent, CRDT merge is correct, decommissioned is tombstone
- [ ] Readiness gate blocks protected writes during warmup
- [ ] Graceful shutdown flushes BatchWriter and leaves SWIM
- [ ] Background tasks restart on panic via JoinSet supervision
- [ ] GET /events is paginated
- [ ] Checksum uses v1.7 canonical format with all fields
- [ ] Docker Compose: 3 nodes + per-node Postgres, events converge
- [ ] min_acks=0 durability limitation is documented
- [ ] CLI covers all v2 endpoints
