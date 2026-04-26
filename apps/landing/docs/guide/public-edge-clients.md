# Public Edge Clients

External developers do not join the private libp2p node mesh.

Instead, they use plain HTTPS against public edge gateways. The edges expose two
bootstrap endpoints:

- `GET /gateway/health`
- `GET /gateway/edges`

## `GET /gateway/health`

This is the cheap liveness and routing-hints endpoint for one edge.

Example fields:

- `edge_id`
- `region`
- `base_url`
- `ready`
- `healthy_nodes`
- `best_node_rtt_ms`
- `sync_gap`
- `overloaded`

## `GET /gateway/edges`

This returns the public regional edge directory only. It does not expose private
full-node addresses or internal mesh identities.

Example response:

```json
{
  "observed_at_unix_ms": 1775770123456,
  "edges": [
    {
      "edge_id": "use1",
      "region": "us-east-1",
      "base_url": "https://use1.api.shardd.xyz",
      "health_url": "https://use1.api.shardd.xyz/gateway/health",
      "reachable": true,
      "ready": true,
      "healthy_nodes": 3,
      "best_node_rtt_ms": 18,
      "sync_gap": 0,
      "overloaded": false
    }
  ]
}
```

## Official clients

The same bootstrap + failover dance is built into the official SDKs, so
typical apps call a couple of high-level methods and skip the plumbing:

::: code-group

```bash [curl]
# no install — curl ships with every system
```

```toml [rust]
# Cargo.toml
[dependencies]
shardd = "0.1"
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

- Rust → [crates.io/shardd](https://crates.io/crates/shardd) · [docs.rs](https://docs.rs/shardd)
- Python → [pypi.org/project/shardd](https://pypi.org/project/shardd/)
- TypeScript → [npmjs.com/package/@shardd/sdk](https://www.npmjs.com/package/@shardd/sdk)
- Kotlin / JVM → [central.sonatype.com/artifact/xyz.shardd/sdk](https://central.sonatype.com/artifact/xyz.shardd/sdk)
- Full source + runnable examples → [shardd-xyz/shardd/tree/main/sdks](https://github.com/shardd-xyz/shardd/tree/main/sdks)

Each SDK accepts a list of bootstrap edges, probes
`/gateway/health`, refreshes `/gateway/edges` on demand, and fails over
on timeout or `5xx`. See the [Quickstart](/guide/quickstart) for the
common calls.
