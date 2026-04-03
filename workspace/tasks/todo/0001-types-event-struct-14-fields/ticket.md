# Event Struct with All 14 Fields

**Source**: brutal-plan
**Plan**: `workspace/plans/PLAN-0001-v2-full-rewrite.md`
**Phase**: 1 — Foundation

## Description
Define the Event struct per §2.1 with all 14 fields. Define `OriginKey` type alias for `(String, u32, u64)` and `EpochKey` for `(String, u32)`. Define `EventType` enum (standard/void/hold_release). Define all request/response types per §7 including `AckInfo`, `CreateEventRequest` (with idempotency_nonce, min_acks, ack_timeout_ms), `CreateEventResponse` (with available_balance, deduplicated, non-nullable acks), error responses. Define `NodeRegistryEntry` with CRDT-mergeable fields.

## Acceptance Criteria
- [ ] Event has all 14 fields from §2.1
- [ ] Dedup key type `OriginKey = (String, u32, u64)` defined
- [ ] `EventType` enum with Serialize/Deserialize
- [ ] All API request/response types match §7
- [ ] `AckInfo::fire_and_forget()` constructor exists
- [ ] `serde(default)` on optional/new fields for backward compat
- [ ] Checksum canonical format per §8.2

## Dependencies
- Blocked by: None
- Blocks: 0002, 0003, 0004, 0005, 0008, 0010-0013

## History
- 2026-04-03 Created from brutal-plan PLAN-0001
