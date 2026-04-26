# Official SDKs

`shardd` ships first-party clients for four ecosystems. Every SDK
implements the same edge probe + automatic failover dance, so the
typical app calls a couple of high-level methods and skips the
`/gateway/health` and `/gateway/edges` plumbing.

| Language | Package | Registry | Source |
|---|---|---|---|
| Rust | `shardd` | [crates.io](https://crates.io/crates/shardd) · [docs.rs](https://docs.rs/shardd) | [sdks/rust](https://github.com/shardd-xyz/shardd/tree/main/sdks/rust) |
| Python | `shardd` | [pypi.org](https://pypi.org/project/shardd/) | [sdks/python](https://github.com/shardd-xyz/shardd/tree/main/sdks/python) |
| TypeScript | `@shardd/sdk` | [npmjs.com](https://www.npmjs.com/package/@shardd/sdk) | [sdks/typescript](https://github.com/shardd-xyz/shardd/tree/main/sdks/typescript) |
| Kotlin / JVM | `xyz.shardd:sdk` | [Maven Central](https://central.sonatype.com/artifact/xyz.shardd/sdk) | [sdks/kotlin](https://github.com/shardd-xyz/shardd/tree/main/sdks/kotlin) |
| CLI | `shardd-cli` (binary `shardd`) | [crates.io](https://crates.io/crates/shardd-cli) | [apps/developer-cli](https://github.com/shardd-xyz/shardd/tree/main/apps/developer-cli) |

## Install

::: code-group

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

```bash [cli]
cargo install shardd-cli
# then:
shardd auth login
```

:::

## What every SDK does

- **Bootstrap** from a list of edge URLs (defaults to the three prod regions: `use1`, `euc1`, `ape1`).
- **Probe** `/gateway/health` on first use, rank by latency, pin the best healthy candidate.
- **Refresh** the regional directory via `/gateway/edges` on demand.
- **Fail over** on `503` / `504` / timeout — cool off the bad edge for 60 seconds, retry once against the next-best candidate, reuse the same idempotency nonce so retries collapse onto the same logical write.
- **Idempotency** — every `createEvent` carries a nonce. Capture it on your side for safe retries; if you don't pass one, the SDK generates a UUID v4.

See the [Quickstart](/guide/quickstart) for the common calls in every
language, or browse the per-SDK README on GitHub for the full surface
(holds, account detail, `listEvents`, custom edges, etc.).
