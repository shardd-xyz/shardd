# Quickstart

`shardd`'s public developer path is HTTPS to regional edge gateways.

Start with one or more bootstrap edge URLs:

- `https://use1.api.shardd.xyz`
- `https://ape1.api.shardd.xyz`
- `https://euc1.api.shardd.xyz`

Get an API key at [app.shardd.xyz → Keys](https://app.shardd.xyz/dashboard/keys)
and export it as `SHARDD_API_KEY`. The tabs below show the same
operations in five forms — pick the one that fits your stack; the
choice sticks across pages.

## Install the SDK

::: code-group

```bash [curl]
# no install — curl ships with every system
```

```toml [rust]
# Cargo.toml
[dependencies]
shardd = "0.1"
tokio  = { version = "1", features = ["full"] }
```

```bash [python]
pip install shardd
```

```bash [typescript]
npm install @shardd/sdk
```

```kotlin [kotlin]
// build.gradle.kts
dependencies {
    implementation("xyz.shardd:sdk:0.1.0")
}
```

:::

## Health check

::: code-group

```bash [curl]
curl -sS https://use1.api.shardd.xyz/gateway/health
```

```rust [rust]
use shardd::Client;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(std::env::var("SHARDD_API_KEY")?)?;
    let health = client.health(None).await?;
    println!("{:?}", health);
    Ok(())
}
```

```python [python]
import os
from shardd import Shardd

shardd = Shardd(os.environ["SHARDD_API_KEY"])
print(shardd.health())
```

```typescript [typescript]
import { Client } from "@shardd/sdk";

const shardd = new Client(process.env.SHARDD_API_KEY!);
console.log(await shardd.health());
```

```kotlin [kotlin]
import xyz.shardd.sdk.Client

val shardd = Client(System.getenv("SHARDD_API_KEY"))
println(shardd.health())
```

:::

## Deposit (credit)

::: code-group

```bash [curl]
curl -sS https://use1.api.shardd.xyz/events \
  -H "Authorization: Bearer $SHARDD_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"bucket":"orders","account":"alice","amount":100,"note":"top-up"}'
```

```rust [rust]
use shardd::{Client, CreateEventOptions};

let result = client
    .create_event(
        "orders",
        "alice",
        100,
        CreateEventOptions { note: Some("top-up".into()), ..Default::default() },
    )
    .await?;
println!("balance = {}", result.balance);
```

```python [python]
result = shardd.create_event("orders", "alice", 100, note="top-up")
print("balance =", result.balance)
```

```typescript [typescript]
const result = await shardd.createEvent("orders", "alice", 100, { note: "top-up" });
console.log("balance =", result.balance);
```

```kotlin [kotlin]
import xyz.shardd.sdk.CreateEventOptions

val result = shardd.createEvent(
    "orders", "alice", 100,
    CreateEventOptions(note = "top-up"),
)
println("balance = ${result.balance}")
```

:::

## Charge (debit)

Negative `amount`, plus an `idempotency_nonce` so retries don't
double-charge. If you omit the nonce the SDK generates a UUID v4 for
you — pass one explicitly when you want retries to collapse onto a
known logical op.

::: code-group

```bash [curl]
curl -sS https://use1.api.shardd.xyz/events \
  -H "Authorization: Bearer $SHARDD_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "bucket": "orders",
    "account": "alice",
    "amount": -25,
    "note": "order-9821",
    "idempotency_nonce": "order-9821-charge"
  }'
```

```rust [rust]
let result = client
    .create_event(
        "orders",
        "alice",
        -25,
        CreateEventOptions {
            note: Some("order-9821".into()),
            idempotency_nonce: Some("order-9821-charge".into()),
            ..Default::default()
        },
    )
    .await?;
println!("balance = {}", result.balance);
```

```python [python]
result = shardd.create_event(
    "orders", "alice", -25,
    note="order-9821",
    idempotency_nonce="order-9821-charge",
)
print("balance =", result.balance)
```

```typescript [typescript]
const result = await shardd.createEvent("orders", "alice", -25, {
  note: "order-9821",
  idempotencyNonce: "order-9821-charge",
});
console.log("balance =", result.balance);
```

```kotlin [kotlin]
val result = shardd.createEvent(
    "orders", "alice", -25,
    CreateEventOptions(
        note = "order-9821",
        idempotencyNonce = "order-9821-charge",
    ),
)
println("balance = ${result.balance}")
```

:::

## Read balances

::: code-group

```bash [curl]
curl -sS "https://use1.api.shardd.xyz/balances?bucket=orders" \
  -H "Authorization: Bearer $SHARDD_API_KEY"
```

```rust [rust]
let balances = client.get_balances("orders").await?;
for row in balances.accounts {
    println!("{} = {}", row.account, row.balance);
}
```

```python [python]
balances = shardd.get_balances("orders")
for row in balances.accounts:
    print(f"{row.account} = {row.balance}")
```

```typescript [typescript]
const balances = await shardd.getBalances("orders");
for (const row of balances.accounts) {
  console.log(`${row.account} = ${row.balance}`);
}
```

```kotlin [kotlin]
val balances = shardd.getBalances("orders")
for (row in balances.accounts) {
    println("${row.account} = ${row.balance}")
}
```

:::
