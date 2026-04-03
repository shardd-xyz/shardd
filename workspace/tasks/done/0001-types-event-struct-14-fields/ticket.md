# Event Struct with All 14 Fields

**Source**: brutal-plan
**Plan**: `workspace/plans/PLAN-0001-v2-full-rewrite.md`
**Phase**: 1 — Foundation

## Description
Define the Event struct per §2.1 with all 14 fields. Define `OriginKey` type alias for `(String, u32, u64)` and `EpochKey` for `(String, u32)`. Define `EventType` enum (standard/void/hold_release). Define all request/response types per §7 including `AckInfo`, `CreateEventRequest` (with idempotency_nonce, min_acks, ack_timeout_ms), `CreateEventResponse` (with available_balance, deduplicated, non-nullable acks), error responses. Define `NodeRegistryEntry` with CRDT-mergeable fields.

## Acceptance Criteria
- [x] Event has all 14 fields from §2.1
- [x] Dedup key type `OriginKey = (String, u32, u64)` defined
- [x] `EventType` enum with Serialize/Deserialize
- [x] All API request/response types match §7
- [x] `AckInfo::fire_and_forget()` constructor exists
- [x] `serde(default)` on optional/new fields for backward compat
- [x] Checksum canonical format per §8.2

## Dependencies
- Blocked by: None
- Blocks: 0002, 0003, 0004, 0005, 0008, 0010-0013

## History
- 2026-04-03 Created from brutal-plan PLAN-0001
- 2026-04-03 05:45 Started work on this task
- 2026-04-03 05:55 Implementation complete: 15 tests, clippy clean
- 2026-04-03 05:55 Task completed. 0 CRITICAL, 0 MAJOR findings.
