# Public HTTPS Edge Clients

`shardd`'s public client transport is HTTPS to regional edge gateways.

Full nodes stay private on the libp2p mesh. External developers do not need the
cluster key and do not connect to full nodes directly.

## Bootstrap model

A client starts with one or more public regional edge URLs:

- `https://use1.api.shardd.xyz`
- `https://ape1.api.shardd.xyz`
- `https://euc1.api.shardd.xyz`

The client then:

1. calls `GET /gateway/edges` on any reachable bootstrap edge
2. merges the bootstrap list with the discovered directory
3. probes `GET /gateway/health` on candidate edges
4. prefers healthy, ready, non-overloaded edges
5. chooses the lowest-latency candidate
6. retries the request against the next best edge on failure

## Public discovery endpoints

### `GET /gateway/health`

Cheap public liveness and routing hints for one edge.

Example response:

```json
{
  "observed_at_unix_ms": 1775770123456,
  "edge_id": "use1",
  "region": "us-east-1",
  "base_url": "https://use1.api.shardd.xyz",
  "ready": true,
  "discovered_nodes": 3,
  "healthy_nodes": 3,
  "best_node_rtt_ms": 18,
  "sync_gap": 0,
  "overloaded": false,
  "auth_enabled": true
}
```

### `GET /gateway/edges`

Public edge directory for SDK bootstrap and refresh.

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
      "observed_at_unix_ms": 1775770123444,
      "discovered_nodes": 3,
      "healthy_nodes": 3,
      "best_node_rtt_ms": 18,
      "sync_gap": 0,
      "overloaded": false
    }
  ]
}
```

## Direct `curl`

If you do not want automatic edge selection, you can just target a named edge:

```bash
curl -sS https://use1.api.shardd.xyz/gateway/health
```

Authenticated write:

```bash
curl -sS https://use1.api.shardd.xyz/events \
  -H "Authorization: Bearer $SHARDD_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "bucket": "orders",
    "account": "alice",
    "amount": 10,
    "note": "{\"kind\":\"credit\",\"source\":\"quickstart\"}"
  }'
```

Read balances:

```bash
curl -sS \
  "https://use1.api.shardd.xyz/balances?bucket=orders" \
  -H "Authorization: Bearer $SHARDD_API_KEY"
```

## Example clients

Runnable examples live under [`docs/examples`](/home/user/Workspaces/shardd/docs/examples):

- Python: [`public_edge_client.py`](/home/user/Workspaces/shardd/docs/examples/public_edge_client.py)
- TypeScript: [`public_edge_client.ts`](/home/user/Workspaces/shardd/docs/examples/public_edge_client.ts)
- Rust: [`docs/examples/rust-edge-client`](/home/user/Workspaces/shardd/docs/examples/rust-edge-client)

Each example:

- accepts multiple bootstrap URLs
- discovers other public edges from `/gateway/edges`
- probes health/latency locally
- picks the best edge
- performs an authenticated request over HTTPS

## Bash fallback

If you want a simple shell fallback with a fixed bootstrap list:

```bash
for edge in \
  https://use1.api.shardd.xyz \
  https://ape1.api.shardd.xyz \
  https://euc1.api.shardd.xyz
do
  echo "== $edge =="
  curl -fsS "$edge/gateway/health" || true
  echo
done
```

That does not do smart directory merging or automatic failover; it is just a
manual inspection path.
