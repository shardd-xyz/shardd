# Cross-Region Benchmark

Benchmark shardd's libp2p networking layer against simulated AWS-region latencies.

## What it measures

- **Per-region throughput** (events/sec accepted by each node)
- **Per-region latency** (p50/p99 of mesh RPC round-trip)
- **Convergence** (do all nodes arrive at the same checksum after load stops?)
- **Event count gap** (max − min across regions at the convergence check)

## Topology

10 nodes across 5 simulated regions, with `tc netem` applying one-way egress delay per node (half of the round-trip). Each node has its own Postgres instance (protocol §6.1 — no shared storage) running on `tmpfs` to keep the DB off the critical path.

| Region          | Nodes | One-way delay | ~RTT |
|-----------------|-------|---------------|------|
| us-east-1       | 2     | 0 ms          | 0    |
| us-west-2       | 2     | 32 ms         | 65   |
| eu-west-1       | 2     | 35 ms         | 69   |
| ap-southeast-1  | 2     | 110 ms        | 219  |
| sa-east-1       | 2     | 58 ms         | 115  |

One node per major region is promoted to seed status (no `BOOTSTRAP` env): `us-east-1a`, `eu-west-1a`, `ap-southeast-1a`. Every non-seed dials two seeds so mesh centrality spreads across three hubs instead of concentrating on one. Kademlia + Identify handle the rest of peer discovery; gossipsub disseminates events. Nodes expose libp2p only; the benchmark talks to them through the mesh client.

## Running

```bash
# Prerequisite: docker + cargo
./run.sh [duration_secs] [concurrency_per_region]

# Defaults: 30s load, 50 concurrent writes/region (500 total)
./run.sh
```

The script:
1. Builds the shardd-node image (first run: several minutes for libp2p dep tree)
2. Starts the 10-node cluster with `docker compose up -d`
3. Waits 15s for libp2p peer discovery + mesh formation
4. Runs `shardd-bench cross-region` against the mesh via a bootstrap peer
5. Tears down the cluster

## Output

```
--- Per-region results ---
  Region                       ok        err        rps   mean(ms)    p50(ms)    p99(ms)
  ------------------------------------------------------------------------------------------
  us-east-1a                 12345          0        412       2.1        1.9        8.4
  ...
  TOTAL                      98765         12       3293

--- Final state per node ---
  Region                    events       checksum[..16]
  --------------------------------------------------------
  us-east-1a                 98765       abc123def4567890
  ...
  ✓ CONVERGED: all nodes have identical checksum
```

## Files

- `Dockerfile.node` — multi-stage build of `shardd-node` + `shardd-bench`
- `entrypoint.sh` — applies `tc netem` delay then launches the libp2p-only node
- `docker-compose.yml` — 10 nodes + 10 per-node Postgres instances, 5 regions, 3-seed topology
- `run.sh` — orchestration script
