# Node Registry CRDT + API Endpoints

**Source**: brutal-plan
**Plan**: `workspace/plans/PLAN-0001-v2-full-rewrite.md`
**Phase**: 3

## Description
Implement per plan Phase 3. See PLAN-0001 for full details.

## Dependencies
- Blocked by: 0003,0015
- Blocks: 0018

## History
- 2026-04-03 Created from brutal-plan PLAN-0001
- 2026-04-03 07:55 Already implemented: CRDT merge in types (NodeRegistryEntry::merge),
  SQL upsert in PostgresStorage (upsert_registry_entry), InMemoryStorage,
  GET /registry + POST /join endpoints in api.rs. Task complete.
