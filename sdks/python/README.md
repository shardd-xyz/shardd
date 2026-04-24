# shardd

[![PyPI](https://img.shields.io/pypi/v/shardd.svg)](https://pypi.org/project/shardd/)
[![Python versions](https://img.shields.io/pypi/pyversions/shardd.svg)](https://pypi.org/project/shardd/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Official Python client for [shardd](https://shardd.xyz) — a globally
distributed credit ledger with a sub-10ms write path in every region.

- **Zero config** — pass an API key; the SDK picks the closest healthy edge.
- **Automatic failover** — transient 5xx/timeouts fall over to the next region, reusing the idempotency nonce so retries collapse.
- **Sync + async** — `Shardd` and `AsyncShardd` share the same method surface.
- **Fully typed** — dataclasses, no runtime introspection magic.

## Install

```bash
pip install shardd
```

## 30-second quickstart

```python
import os
from shardd import Shardd

shardd = Shardd(os.environ["SHARDD_API_KEY"])

# Credit 500 units to user:alice in the `my-app` bucket.
result = shardd.create_event("my-app", "user:alice", 500)
print("new balance =", result.balance)

# Read back the whole bucket.
balances = shardd.get_balances("my-app")
for row in balances.accounts:
    print(f"{row.account} = {row.balance}")
```

Get an API key at <https://app.shardd.xyz> → **Keys**.

## Async

```python
import asyncio
from shardd import AsyncShardd

async def main():
    async with AsyncShardd(api_key) as shardd:
        result = await shardd.create_event("my-app", "user:alice", -100)
        print(result.balance)

asyncio.run(main())
```

## API

| Method (sync & async) | Purpose |
|---|---|
| `Shardd(api_key, *, edges=None, timeout_s=30.0, http=None)` | Build a client. |
| `create_event(bucket, account, amount, *, note=None, idempotency_nonce=None, max_overdraft=None, min_acks=None, ack_timeout_ms=None, hold_amount=None, hold_expires_at_unix_ms=None)` | Charge, credit, reserve, or release balance. |
| `charge(bucket, account, amount, **kw)` | Debit sugar. |
| `credit(bucket, account, amount, **kw)` | Credit sugar. |
| `list_events(bucket)` | Event history for a bucket. |
| `get_balances(bucket)` | All balances in a bucket. |
| `get_account(bucket, account)` | One account's balance + holds. |
| `edges()` | Current regional directory. |
| `health(base_url=None)` | Pinned (or specified) edge's health snapshot. |

## Idempotency

Every `create_event` carries an `idempotency_nonce`. If you don't supply
one, the SDK generates a UUID v4 for you. For safe retries, capture the
nonce client-side and reuse it:

```python
import uuid

nonce = str(uuid.uuid4())
result = shardd.create_event(
    "my-app", "user:alice", -100,
    note="order #9821",
    idempotency_nonce=nonce,
)
# A retry with the same `nonce` returns the original event and
# `result.deduplicated is True` — no double charge.
```

## Failover behavior

The three prod regions (`use1.api.shardd.xyz`, `euc1.api.shardd.xyz`,
`ape1.api.shardd.xyz`) are baked in as defaults. On the first request
the client parallel-probes `/gateway/health` on all three, picks the
lowest-latency healthy one, and pins it. If that edge returns `503`/
`504`/timeouts/connect-errors, the SDK marks it unavailable for 60s
and retries the request once against the next-best candidate.
Non-retryable errors (`400`, `401`, `403`, `404`, `422`) surface
immediately — no retry, no failover.

Override the edges for local or self-hosted clusters:

```python
shardd = Shardd(
    api_key,
    edges=[
        "http://localhost:8081",
        "http://localhost:8082",
        "http://localhost:8083",
    ],
)
```

## Error handling

```python
from shardd import Shardd, InsufficientFundsError, ShardError

try:
    shardd.create_event("my-app", "user:alice", -1000)
except InsufficientFundsError as err:
    print(f"short {1000 - err.available_balance} credits")
except ShardError as err:
    if err.retryable:
        # queue for retry
        ...
    else:
        raise
```

## License

MIT © shardd
