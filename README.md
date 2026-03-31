# shardd

Multi-writer replicated append-only event system.

Each node owns a local append-only sublog with gapless sequence numbers.
Events are stored in a per-node PostgreSQL database; only balances and heads
are kept in memory. Replication is eventual — nodes sync by exchanging
per-origin contiguous heads and fetching missing suffix ranges over HTTP+JSON.

## Project structure

```
apps/
  node/       shardd-node      — main node binary (API server + sync loop)
  cli/        shardd-cli       — CLI client for interacting with running nodes
  dashboard/  shardd-dashboard — web dashboard (Dioxus)
libs/
  types/      shardd-types     — shared data types
  storage/    shardd-storage   — Postgres-backed persistence layer
              storage/postgres.rs      — PostgresStorage (production)
              storage/memory.rs        — InMemoryStorage (tests)
              storage/migrations/      — SQL migrations
infra/        production deployment (compose, caddy, deploy scripts)
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

# Or run nodes individually:
DATABASE_URL=postgres://shardd:shardd@localhost/shardd_a ./run node 3001
DATABASE_URL=postgres://shardd:shardd@localhost/shardd_b ./run node 3002 --bootstrap 127.0.0.1:3001
DATABASE_URL=postgres://shardd:shardd@localhost/shardd_c ./run node 3003 --bootstrap 127.0.0.1:3001
```

## Using the CLI

```bash
# Create events
./run cli --node http://127.0.0.1:3001 create-event --amount 10 --note "from A"
./run cli --node http://127.0.0.1:3002 create-event --amount -3 --note "from B"
./run cli --node http://127.0.0.1:3003 create-event --amount 7 --note "from C"

# Check state
./run cli --node http://127.0.0.1:3001 state
./run cli --node http://127.0.0.1:3002 state
./run cli --node http://127.0.0.1:3003 state

# Other commands
./run cli health
./run cli peers
./run cli events
./run cli heads
./run cli sync
```

## Using curl

```bash
# Create event
curl -s -X POST localhost:3001/events \
  -H 'content-type: application/json' \
  -d '{"amount": 10, "note": "from A"}' | jq .

# Check convergence
curl -s localhost:3001/state | jq '{event_count, balance, checksum}'
curl -s localhost:3002/state | jq '{event_count, balance, checksum}'
curl -s localhost:3003/state | jq '{event_count, balance, checksum}'

# Collapsed balances (all buckets/accounts)
curl -s localhost:3001/collapsed | jq .

# Collapsed balance for a specific bucket/account
curl -s localhost:3001/collapsed/default/alice | jq .
```

## Dashboard

```bash
# Install dioxus CLI (one time)
cargo install dioxus-cli

# Start cluster, then serve dashboard with hot-reload
./run cluster
./run dashboard
# Opens at http://localhost:8080 — enter http://127.0.0.1:3001 as bootstrap
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
| `cli [args]` | Run the CLI client |
| `dashboard` | Serve dashboard (hot-reload) |
| `dashboard:build` | Build dashboard for production |
| `build` | Build Docker image |
| `fmt` | Format all Rust code |
| `lint` | Run clippy |
| `test` | Run all tests |
| `deploy` | Build and deploy to production |
| `clean` | Remove demo data |

## Node CLI flags

| Flag | Default | Description |
|---|---|---|
| `--host` | `127.0.0.1` | Listen host |
| `--port` | (required) | Listen port |
| `--advertise-addr` | `host:port` | Address advertised to peers |
| `--database-url` | `DATABASE_URL` env | Postgres connection string |
| `--bootstrap` | (none) | Bootstrap peer(s), repeatable |
| `--fanout` | `3` | Peers per sync round |
| `--sync-interval-ms` | `3000` | Sync interval |
| `--max-peers` | `16` | Max tracked peers |

The `--database-url` flag (or `DATABASE_URL` environment variable) replaces the
old `--config-dir` flag. Each node must point to its own Postgres database.

## Storage

Events are persisted in a per-node PostgreSQL database. Only aggregated balances
and per-origin heads are held in memory (`SharedState<S>` is generic over
`StorageBackend`). The SQL schema is applied automatically on startup from
`libs/storage/migrations/001_create_ledger_tables.sql`.

Checksum format: `origin:seq:event_id:bucket:account:amount` lines joined by `\n`.

## Production deployment

See [infra/README.md](infra/README.md) for production deployment with Docker + Caddy.
