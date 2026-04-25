# shardd Kotlin SDK

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Official Kotlin / JVM client for [shardd](https://shardd.xyz) — a
globally distributed credit ledger with automatic regional failover.

- **One-line setup** — pass an API key; the SDK picks the closest healthy edge.
- **Automatic failover** — transient 5xx and timeout failures fall over to the next region, reusing the same idempotency nonce so retries collapse.
- **JVM-friendly** — plain blocking API, Java 11+ compatible, no coroutine dependency required.

## Install

Gradle:

```kotlin
implementation("xyz.shardd:sdk:0.1.0")
```

Maven:

```xml
<dependency>
  <groupId>xyz.shardd</groupId>
  <artifactId>sdk</artifactId>
  <version>0.1.0</version>
</dependency>
```

## Quickstart

```kotlin
import xyz.shardd.sdk.Client

fun main() {
    val shardd = Client(System.getenv("SHARDD_API_KEY"))

    val result = shardd.createEvent("my-app", "user:alice", 500)
    println("new balance = ${result.balance}")

    val balances = shardd.getBalances("my-app")
    for (row in balances.accounts) {
        println("${row.account} = ${row.balance}")
    }
}
```

## API

| Method | Purpose |
|---|---|
| `Client(apiKey, options)` | Build a client. `ClientOptions` may override `edges` or `timeoutMillis`. |
| `createEvent(bucket, account, amount, opts)` | Charge, credit, reserve, or release balance. Positive amount = credit, negative = debit. |
| `charge(bucket, account, amount, opts)` | Debit sugar. |
| `credit(bucket, account, amount, opts)` | Credit sugar. |
| `listEvents(bucket)` | Event history for a bucket. |
| `getBalances(bucket)` | All balances in a bucket. |
| `getAccount(bucket, account)` | One account's balance + holds. |
| `edges()` | Current regional directory. |
| `health(baseUrl)` | Pinned (or specified) edge's health snapshot. |

## Idempotency

Every `createEvent` carries an `idempotencyNonce`. If you do not supply
one, the SDK generates a UUID. For safe retries, capture the nonce on
your side and reuse it:

```kotlin
import xyz.shardd.sdk.CreateEventOptions
import java.util.UUID

val nonce = UUID.randomUUID().toString()
val result = shardd.createEvent(
    bucket = "my-app",
    account = "user:alice",
    amount = -100,
    opts = CreateEventOptions(
        note = "order #9821",
        idempotencyNonce = nonce,
    ),
)
```

## Failover behavior

The three prod regions (`use1.api.shardd.xyz`, `euc1.api.shardd.xyz`,
`ape1.api.shardd.xyz`) are the defaults. On first use the client probes
all configured edges, ranks them by observed latency, and pins the best
healthy candidate. If that edge later returns `503`/`504` or times out,
the SDK cools it off for 60 seconds and retries the request once against
the next-best candidate.

Override the edges for local or self-hosted clusters:

```kotlin
import xyz.shardd.sdk.Client
import xyz.shardd.sdk.ClientOptions

val shardd = Client(
    apiKey = System.getenv("SHARDD_API_KEY"),
    options = ClientOptions(
        edges = listOf(
            "http://localhost:8081",
            "http://localhost:8082",
            "http://localhost:8083",
        ),
    ),
)
```

## Publishing

Releases go to Maven Central via the Sonatype Central Portal, driven
from the operator machine:

```bash
./run sdk:publish:kotlin 0.1.1
```

That auto-sources `infra/secrets/sdk-publish.env` for the Central
Portal user token and the ASCII-armored GPG private key, and prompts
interactively for the GPG passphrase (which is intentionally not
stored on disk).

The env file sets these Gradle properties (Gradle picks them up via
`ORG_GRADLE_PROJECT_*`):

- `mavenCentralUsername` — Central Portal user token name
- `mavenCentralPassword` — Central Portal user token
- `signingInMemoryKey` — ASCII-armored GPG private key

`./run sdk:test:kotlin` runs `publishToMavenLocal` without signing, so
day-to-day testing does not need any credentials.

## License

MIT © shardd
