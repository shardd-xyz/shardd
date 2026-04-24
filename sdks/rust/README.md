# shardd

[![Crates.io](https://img.shields.io/crates/v/shardd.svg)](https://crates.io/crates/shardd)
[![Docs.rs](https://docs.rs/shardd/badge.svg)](https://docs.rs/shardd)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Official Rust client for [shardd](https://shardd.xyz) — a globally
distributed credit ledger with a sub-10ms write path in every region.

- **One-line setup** — pass an API key; the SDK picks the closest healthy edge.
- **Automatic failover** — transient 5xx/timeouts fall over to the next region, re-using the same idempotency nonce so retries collapse.
- **Safe by default** — every write is auto-deduped.

## Install

```toml
[dependencies]
shardd = "0.1"
tokio  = { version = "1", features = ["full"] }
```

## 30-second quickstart

```rust
use shardd::{Client, CreateEventOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(std::env::var("SHARDD_API_KEY")?)?;

    // Credit 500 units to user:alice in the `my-app` bucket.
    let result = client
        .create_event("my-app", "user:alice", 500, Default::default())
        .await?;
    println!("new balance = {}", result.balance);

    // Read back the whole bucket.
    let balances = client.get_balances("my-app").await?;
    for row in balances.accounts {
        println!("{} = {}", row.account, row.balance);
    }
    Ok(())
}
```

Get an API key at <https://app.shardd.xyz> → **Keys**.

## API

| Method | Purpose |
|---|---|
| [`Client::new`](https://docs.rs/shardd/*/shardd/struct.Client.html#method.new) | Build a client with an API key and prod defaults. |
| [`Client::builder`](https://docs.rs/shardd/*/shardd/struct.Client.html#method.builder) | Override edges, timeout, or HTTP client. |
| [`Client::create_event`](https://docs.rs/shardd/*/shardd/struct.Client.html#method.create_event) | Charge, credit, reserve, or release balance. Positive amount = credit, negative = debit. |
| [`Client::charge`](https://docs.rs/shardd/*/shardd/struct.Client.html#method.charge) | Sugar for a plain debit. |
| [`Client::credit`](https://docs.rs/shardd/*/shardd/struct.Client.html#method.credit) | Sugar for a plain credit. |
| [`Client::list_events`](https://docs.rs/shardd/*/shardd/struct.Client.html#method.list_events) | Event history for a bucket. |
| [`Client::get_balances`](https://docs.rs/shardd/*/shardd/struct.Client.html#method.get_balances) | All balances in a bucket. |
| [`Client::get_account`](https://docs.rs/shardd/*/shardd/struct.Client.html#method.get_account) | One account's balance + holds. |
| [`Client::edges`](https://docs.rs/shardd/*/shardd/struct.Client.html#method.edges) | Current regional directory. |
| [`Client::health`](https://docs.rs/shardd/*/shardd/struct.Client.html#method.health) | Pinned (or specified) edge's health snapshot. |

Full reference: [docs.rs/shardd](https://docs.rs/shardd).

## Idempotency

Every `create_event` carries an `idempotency_nonce`. If you don't supply
one, the SDK generates a UUID v4. To make retries safe, capture the
nonce on your side and reuse it:

```rust
use shardd::CreateEventOptions;

let nonce = uuid::Uuid::new_v4().to_string();
let result = client
    .create_event(
        "my-app",
        "user:alice",
        -100,
        CreateEventOptions {
            idempotency_nonce: Some(nonce.clone()),
            note: Some("order #9821".into()),
            ..Default::default()
        },
    )
    .await?;
// A retry with the same `nonce` returns the original event with
// `result.deduplicated == true` — no double charge.
```

## Failover behavior

The three prod regions (`use1.api.shardd.xyz`, `euc1.api.shardd.xyz`,
`ape1.api.shardd.xyz`) are baked in as defaults. On the first request
the client parallel-probes `/gateway/health` against all three, picks
the lowest-latency healthy one, and sticks with it for the lifetime of
the `Client`. On `503`/`504`/timeouts/connect-errors the SDK marks the
failing edge unavailable for 60 seconds and retries the request once
against the next-best candidate. Non-retryable errors (`400`, `401`,
`403`, `404`, `422`) surface immediately.

Override the edges for local or self-hosted clusters:

```rust
let client = Client::builder()
    .api_key(key)
    .edges(vec![
        "http://localhost:8081".into(),
        "http://localhost:8082".into(),
        "http://localhost:8083".into(),
    ])
    .build()?;
```

## TLS

The default feature set uses `rustls`. Switch to `native-tls` with:

```toml
shardd = { version = "0.1", default-features = false, features = ["native-tls"] }
```

## License

MIT © shardd
