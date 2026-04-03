# PostgresStorage Implementation

**Source**: brutal-plan
**Plan**: `workspace/plans/PLAN-0001-v2-full-rewrite.md`
**Phase**: 1 — Foundation

## Description
Implement StorageBackend trait for PostgresStorage using sqlx. All queries updated for v2 schema: epoch-aware inserts, bulk insert with ON CONFLICT, aggregate_balances, sequences_by_origin (per epoch), origin_account_mapping (per epoch), CRDT registry merge upsert, matview refresh, idempotency nonce lookups, hold-related queries.

## Acceptance Criteria
- [x] insert_event with ON CONFLICT on 3-column dedup key
- [x] insert_events_bulk for batch writer
- [x] Epoch-aware queries for heads, sequences, ranges
- [x] Registry CRDT merge as single SQL upsert (§14.3)
- [x] Idempotency nonce lookup query
- [x] Active holds + released holds queries for startup rebuild
- [x] run_migrations() + refresh_balance_summary()

## Dependencies
- Blocked by: 0001, 0002
- Blocks: 0005, 0009

## History
- 2026-04-03 Created from brutal-plan PLAN-0001
- 2026-04-03 06:10 Implementation complete. 20 StorageBackend methods + PostgresStorage.
- 2026-04-03 06:10 Task completed. Compiles clean.
