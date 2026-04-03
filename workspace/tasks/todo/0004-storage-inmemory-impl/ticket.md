# InMemoryStorage Implementation

**Source**: brutal-plan
**Plan**: `workspace/plans/PLAN-0001-v2-full-rewrite.md`
**Phase**: 1 — Foundation

## Description
Implement StorageBackend for InMemoryStorage with full v2 semantics: 3-column dedup, idempotency nonce lookups, hold queries, registry CRDT merge, epoch-aware heads. Must match PostgresStorage behavior exactly for tests.

## Acceptance Criteria
- [ ] All StorageBackend methods implemented with correct v2 semantics
- [ ] Dedup by (origin_node_id, origin_epoch, origin_seq)
- [ ] Idempotency nonce conflict detection
- [ ] Registry CRDT merge (decommissioned tombstone)

## Dependencies
- Blocked by: 0001, 0002
- Blocks: 0005 (tests), 0024

## History
- 2026-04-03 Created from brutal-plan PLAN-0001
