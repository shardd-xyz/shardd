# Database Migrations for v2 Schema

**Source**: brutal-plan
**Plan**: `workspace/plans/PLAN-0001-v2-full-rewrite.md`
**Phase**: 1 — Foundation

## Description
Write SQL migrations per §6.1: events table (14 columns, unique index on (origin_node_id, origin_epoch, origin_seq), partial indexes for void_ref and idempotency_nonce), node_meta (with current_epoch), node_registry (permanent, 5 fields), balance_summary materialized view (optional). Drop old peers table.

## Acceptance Criteria
- [ ] events table has all 14 columns
- [ ] Unique index on (origin_node_id, origin_epoch, origin_seq)
- [ ] Partial index on (idempotency_nonce, bucket, account, amount) WHERE NOT NULL
- [ ] Partial index on (void_ref) WHERE NOT NULL
- [ ] node_meta has current_epoch column
- [ ] node_registry table with permanent rows
- [ ] balance_summary materialized view
- [ ] Migrations run cleanly on fresh Postgres

## Dependencies
- Blocked by: 0001
- Blocks: 0003, 0004, 0009

## History
- 2026-04-03 Created from brutal-plan PLAN-0001
