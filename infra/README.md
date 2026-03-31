# shardd infrastructure

## Production deployment

### Prerequisites

- Docker on the target host
- SSH access to the target host
- Caddy image available (pulled automatically)

### Setup

1. Copy `.env.example` to `.env` and fill in values:
   ```
   cp .env.example .env
   ```

2. Configure firewall:
   ```
   sudo ./setup_firewall.sh
   ```

3. Deploy:
   ```
   ./deploy.sh
   ```

This builds the Docker image locally, transfers it to the remote host, and starts a 3-node cluster with Caddy as reverse proxy.

### Architecture

```
Internet → Caddy (:80/:443) → node1:3001 → db1 (Postgres)
                              → node2:3002 → db2 (Postgres)
                              → node3:3003 → db3 (Postgres)
```

Each node has its own dedicated PostgreSQL instance for event storage (configured
via `DATABASE_URL`). Only balances and per-origin heads are kept in memory; events
are queried from Postgres on demand. Nodes sync with each other over HTTP — the
sync protocol is unchanged.

Caddy load-balances across all nodes with health checks on `/health`. All nodes are interchangeable for reads; writes go to whichever node receives them and replicate via the sync protocol.

### Files

| File | Purpose |
|---|---|
| `compose.yml` | Production Docker Compose (3 nodes + Caddy) |
| `deploy.sh` | Build + ship + start |
| `setup_firewall.sh` | UFW rules |
| `caddy/Caddyfile` | Reverse proxy config |
| `.env.example` | Environment variable template |
| `secrets/` | Sensitive files (gitignored) |
