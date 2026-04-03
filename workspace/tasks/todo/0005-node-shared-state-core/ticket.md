# SharedState Core with Per-Account Mutex

**Source**: brutal-plan
**Plan**: `workspace/plans/PLAN-0001-v2-full-rewrite.md`
**Phase**: 1 — Foundation

## Description
Implement SharedState with all 11 in-memory caches per §5. Per-account atomic section via `DashMap<BalanceKey, Arc<Mutex<AccountState>>>`. AccountState holds balance, holds Vec, released HashSet. Entry-level lock on event_buffer for replication dedup safety. In-memory head advancement per (origin, epoch) with pending_seqs.

Critical: create_local_event must hold the per-account mutex across idempotency check + overdraft + hold reservation + event creation (§3.1).

## Acceptance Criteria
- [ ] All 11 caches from §5 implemented
- [ ] Per-account Mutex<AccountState> for the atomic section
- [ ] create_local_event: idempotency + overdraft + hold in single lock
- [ ] insert_event: entry-level dedup on event_buffer
- [ ] Head advancement per (origin, epoch) with pending_seqs drain
- [ ] available_balance computation (balance - active_holds)
- [ ] Balance = SUM(amount) invariant maintained

## Dependencies
- Blocked by: 0001, 0003/0004
- Blocks: 0006, 0007, 0008, 0010-0013, 0018, 0019

## History
- 2026-04-03 Created from brutal-plan PLAN-0001
