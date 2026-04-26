# Architecture

`shardd` splits the system into two planes.

## Private state plane

- full nodes
- local Postgres per full node
- private libp2p mesh
- replication, sync, and convergence

These nodes are correctness-critical and should stay private.

## Public edge plane

- regional edge gateways
- public HTTPS
- developer API key auth
- public edge discovery
- full-node selection based on live mesh health

The edge gateway is not a dumb proxy. It participates in the private mesh
enough to maintain a live routing view, but it holds no shard state of its own.

## Why this is cleaner

- public developers do not need libp2p credentials
- full nodes do not become an internet-facing auth surface
- client tooling stays simple: HTTPS plus a bearer key
- routing decisions use live mesh state, not just DNS

## Deployment shape

Typical production layout:

- a few full-node regions
- more public edge regions than full-node regions
- one separate control-plane/dashboard host

Typical dev layout:

- 3 full nodes
- 3 public edges
- 1 dashboard host
