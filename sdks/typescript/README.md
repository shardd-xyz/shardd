# shardd

[![npm](https://img.shields.io/npm/v/shardd.svg)](https://www.npmjs.com/package/shardd)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Official TypeScript / JavaScript client for
[shardd](https://shardd.xyz) — a globally distributed credit ledger
with a sub-10ms write path in every region. Works in Node 18+ and
modern browsers.

- **Zero config** — pass an API key; the SDK picks the closest healthy edge.
- **Automatic failover** — transient 5xx/timeouts fall over to the next region, reusing the idempotency nonce so retries collapse.
- **Safe by default** — every write is auto-deduped.
- **Tiny** — one runtime dep (`uuid`), dual ESM + CJS, first-class types.

## Install

```bash
npm install shardd
```

## 30-second quickstart

```ts
import { Client } from "shardd";

const shardd = new Client(process.env.SHARDD_API_KEY!);

// Credit 500 units to user:alice in the `my-app` bucket.
const result = await shardd.createEvent("my-app", "user:alice", 500);
console.log("new balance =", result.balance);

// Read back the whole bucket.
const balances = await shardd.getBalances("my-app");
for (const row of balances.accounts) {
  console.log(`${row.account} = ${row.balance}`);
}
```

Get an API key at <https://app.shardd.xyz> → **Keys**.

## API

| Method | Purpose |
|---|---|
| `new Client(apiKey, opts?)` | Build a client. `opts` may override `edges`, `timeoutMs`, or `fetch`. |
| `createEvent(bucket, account, amount, opts?)` | Charge, credit, reserve, or release balance. Positive amount = credit, negative = debit. |
| `charge(bucket, account, amount, opts?)` | Sugar for a plain debit. |
| `credit(bucket, account, amount, opts?)` | Sugar for a plain credit. |
| `listEvents(bucket)` | Event history for a bucket. |
| `getBalances(bucket)` | All balances in a bucket. |
| `getAccount(bucket, account)` | One account's balance + holds. |
| `edges()` | Current regional directory. |
| `health(baseUrl?)` | Pinned (or specified) edge's health snapshot. |

## Idempotency

Every `createEvent` carries an `idempotency_nonce`. If you don't supply
one via `opts.idempotencyNonce`, the SDK generates a UUID v4 for you.
For safe retries, capture the nonce client-side and reuse it:

```ts
import { randomUUID } from "node:crypto";

const nonce = randomUUID();
const result = await shardd.createEvent("my-app", "user:alice", -100, {
  note: "order #9821",
  idempotencyNonce: nonce,
});
// A retry with the same `nonce` returns the original event and
// `result.deduplicated === true` — no double charge.
```

## Failover behavior

The three prod regions (`use1.api.shardd.xyz`, `euc1.api.shardd.xyz`,
`ape1.api.shardd.xyz`) are baked in as defaults. On the first request
the client parallel-probes `/gateway/health` across all three, picks
the lowest-latency healthy one, and pins it. If that edge returns
`503`/`504`/timeouts/connect-errors, the SDK marks it unavailable for
60s and retries the request once against the next-best candidate.
Non-retryable errors (`400`, `401`, `403`, `404`, `422`) surface
immediately — no retry, no failover.

Override the edges for local or self-hosted clusters:

```ts
const shardd = new Client(process.env.SHARDD_API_KEY!, {
  edges: [
    "http://localhost:8081",
    "http://localhost:8082",
    "http://localhost:8083",
  ],
});
```

## Error handling

```ts
import { Client, InsufficientFundsError, ShardError } from "shardd";

try {
  await shardd.createEvent("my-app", "user:alice", -1000);
} catch (err) {
  if (err instanceof InsufficientFundsError) {
    console.log(`short ${-err.availableBalance + 1000} credits`);
  } else if (err instanceof ShardError && err.retryable) {
    // queue for retry
  } else {
    throw err;
  }
}
```

## License

MIT © shardd
