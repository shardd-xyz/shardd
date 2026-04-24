# shardd

Multi-writer replicated append-only event system.

Each node owns a local append-only sublog with gapless sequence numbers.
Events are stored in a per-node PostgreSQL database; only balances and heads
are kept in memory. Replication is eventual — nodes broadcast new events via
libp2p gossipsub and catch up on gaps by exchanging per-origin contiguous
heads and fetching missing suffix ranges over libp2p request-response.

## Project structure

```
apps/
  node/       shardd-node      — libp2p-only node binary
  gateway/    shardd-gateway   — HTTP edge gateway backed by the libp2p mesh
  cli/        shardd-cli       — CLI client for interacting with the mesh
  bench/      shardd-bench     — load testing and convergence verification
libs/
  broadcast/  shardd-broadcast — libp2p transport, mesh client, discovery
  types/      shardd-types     — shared data types
  storage/    shardd-storage   — Postgres-backed persistence layer
              storage/postgres.rs      — PostgresStorage (production)
              storage/memory.rs        — InMemoryStorage (tests)
              storage/migrations/      — SQL migrations
infra/        deployment bundles, local machine state, and infractl
```

## Prerequisites

- Rust toolchain (stable)
- PostgreSQL (local instance or Docker)
- `DATABASE_URL` environment variable pointing to a Postgres database,
  e.g. `postgres://shardd:shardd@localhost/shardd`

## Quick start

```bash
# Build everything
cargo build --workspace --release

# Start a local Postgres (if you don't already have one running)
docker run -d --name shardd-pg -p 5432:5432 \
  -e POSTGRES_DB=shardd -e POSTGRES_USER=shardd -e POSTGRES_PASSWORD=shardd \
  postgres:17-alpine

# Run a 3-node local cluster (each node gets its own database)
./run cluster

# Or run nodes individually. Nodes are libp2p-only; use the gateway for HTTP.
DATABASE_URL=postgres://shardd:shardd@localhost/shardd_a \
  ./run node 4001
DATABASE_URL=postgres://shardd:shardd@localhost/shardd_b \
  ./run node 4002 --bootstrap /ip4/127.0.0.1/tcp/4001
DATABASE_URL=postgres://shardd:shardd@localhost/shardd_c \
  ./run node 4003 --bootstrap /ip4/127.0.0.1/tcp/4001

# Start the HTTP edge gateway
./run gateway 8080
```

## Using the CLI

```bash
# Create events via the mesh
./run cli --bootstrap-peer /ip4/127.0.0.1/tcp/4001 create-event \
  --bucket default --account alice --amount 10 --note "from A"
./run cli --bootstrap-peer /ip4/127.0.0.1/tcp/4001 create-event \
  --bucket default --account alice --amount -3 --note "from B"
./run cli --bootstrap-peer /ip4/127.0.0.1/tcp/4001 create-event \
  --bucket default --account alice --amount 7 --note "from C"

# Check state
./run cli --bootstrap-peer /ip4/127.0.0.1/tcp/4001 state

# Other commands
./run cli --bootstrap-peer /ip4/127.0.0.1/tcp/4001 health
./run cli --bootstrap-peer /ip4/127.0.0.1/tcp/4001 events
./run cli --bootstrap-peer /ip4/127.0.0.1/tcp/4001 heads
```

## Using curl

```bash
# Create event through the gateway
curl -s -X POST localhost:8080/events \
  -H 'content-type: application/json' \
  -d '{"bucket":"default","account":"alice","amount":10,"note":"from A"}' | jq .

# Check convergence
curl -s localhost:8080/state | jq '{event_count, total_balance, checksum}'

# Collapsed balances (all buckets/accounts)
curl -s localhost:8080/collapsed | jq .

# Collapsed balance for a specific bucket/account
curl -s localhost:8080/collapsed/default/alice | jq .
```

## Docker

Each node runs alongside its own Postgres container (see `docker-compose.yml`).

```bash
# Build image
./run build

# Run 3-node cluster in Docker (provisions per-node Postgres automatically)
./run infra

# Tail logs
./run infra:logs

# Stop
./run infra:stop
```

## All commands

```
./run help
```

| Command | Description |
|---|---|
| `infra` | Start 3-node Docker cluster |
| `infra:stop` | Stop containers |
| `infra:logs` | Tail container logs |
| `node [port]` | Run a single node locally |
| `cluster` | Build and run 3-node local cluster |
| `cluster:stop` | Stop local cluster |
| `gateway [port]` | Run the HTTP edge gateway |
| `cli [args]` | Run the CLI client |
| `bench [args]` | Run the mesh benchmark suite |
| `build` | Build node and gateway Docker images |
| `fmt` | Format all Rust code |
| `lint` | Run clippy |
| `test` | Run all tests |
| `deploy` | Build and deploy to production |
| `clean` | Remove demo data |

## Node CLI flags

| Flag | Default | Description |
|---|---|---|
| `--host` | `127.0.0.1` | libp2p listen host |
| `--advertise-addr` | *(unset → `/ip4/{host}/tcp/{libp2p_port}`)* | Libp2p multiaddr advertised to peers/clients |
| `--database-url` | `DATABASE_URL` env | Postgres connection string |
| `--bootstrap` | (none) | libp2p bootstrap multiaddr(s), repeatable (e.g. `/ip4/1.2.3.4/tcp/9000`) |
| `--libp2p-port` | `9000` | libp2p TCP port |
| `--psk-file` | (none) | Path to 32-byte PSK file for libp2p private mesh encryption |
| `--event-worker-count` | `4` | Parallel workers draining gossipsub events (JSON decode + state insert) |
| `--batch-flush-interval-ms` | `100` | BatchWriter flush cadence |
| `--batch-flush-size` | `1000` | BatchWriter flush threshold |
| `--matview-refresh-ms` | `5000` | Materialized view refresh interval |
| `--orphan-check-interval-ms` | `500` | Orphan detector sweep interval |
| `--orphan-age-ms` | `500` | Age at which an unpersisted event is considered orphaned |
| `--hold-multiplier` | `5` | Debit hold multiplier (§11) |
| `--hold-duration-ms` | `600000` | Debit hold expiry |

Each node must point to its own Postgres database.

## Storage

Events are persisted in a per-node PostgreSQL database. Only aggregated balances
and per-origin heads are held in memory (`SharedState<S>` is generic over
`StorageBackend`). The SQL schema is applied automatically on startup from
`libs/storage/migrations/001_create_v2_schema.sql`.

Checksum format: `origin:seq:event_id:bucket:account:amount` lines joined by `\n`.

## Production deployment

Production-style deployment is split into:

- cloud resources: Terraform-managed AWS + Cloudflare
- machine setup: verify/repair host bootstrap over SSH
- service deployment: render and apply bundle-based Compose stacks

See [infra/README.md](infra/README.md) for the current `infractl.py` workflow,
including the topology with full nodes, edge nodes, and a separate
dashboard host. The intended operator entrypoint is `./run`, for example:

```bash
./run state init
./run infra:init --deployment prod
./run infra:plan --deployment prod
./run infra:apply --deployment prod
./run servers setup --deployment prod
./run deploy --deployment prod
```

## License

This repository uses split licensing:

- **Client SDKs under `sdks/`** — the `shardd` crate on
  [crates.io](https://crates.io/crates/shardd), the `@shardd/sdk`
  [npm](https://www.npmjs.com/package/@shardd/sdk) package, the
  `shardd` [PyPI](https://pypi.org/project/shardd/) package, and the
  Kotlin SDK in `sdks/kotlin/` — are MIT. Embed them in any
  application, commercial or otherwise; each SDK subdirectory ships its
  own MIT `LICENSE` file.
- **The `landing/` site and docs** are MIT under [`landing/LICENSE`](landing/LICENSE).
- **Everything else** — the nodes, gateway, dashboard, infra, CLI,
  bench, root docs, and internal libraries — is
  the [TQDM Source-Available License 1.0](LICENSE) with a 4-year future
  grant to Apache 2.0.

The root source-available license always allows reading, modifying,
redistributing, and non-production use. Production use is allowed without a separate
commercial license only if you are below both thresholds on a
consolidated basis across affiliates:

- annual gross revenue of not more than US$10,000,000; and
- fewer than 100 personnel (employees plus individual contractors).

Even below those thresholds, the free grant does not allow using shardd
to offer a competing hosted, managed, API, PaaS, embedded, or OEM
product. Above either threshold, or for a competing offering, you need a
separate commercial license from TQDM Inc. Every version of the root
Licensed Work auto-converts to Apache 2.0 on the fourth anniversary of
its first public distribution.
