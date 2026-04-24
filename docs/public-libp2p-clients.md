# Public libp2p client status

## Current supported public transport

`shardd-node` instances are part of a private libp2p mesh protected by the
shared cluster key (`SHARDD_CLUSTER_KEY`). That is appropriate for node-to-node
and gateway-to-node traffic, but it means external developers cannot safely use
the node mesh directly with only an API key.

Today:

- nodes require the mesh cluster key to join
- external developer auth is enforced at the gateway via the shardd control plane
- the public transport is HTTPS to `shardd-gateway`
- public client bootstrap and failover should use regional public edge URLs
- the current public client contract is documented in [`public-edge-clients.md`](/home/user/Workspaces/shardd/docs/public-edge-clients.md)

## Why direct public node clients are the wrong target

If external clients connect directly to nodes:

- they would need the cluster key, which defeats mesh isolation
- every node would become a public auth surface
- public client protocol changes would be coupled to node internals
- operational rate limiting and abuse control would be spread across the mesh

## Public libp2p status

Public libp2p clients are deferred.

The supported external-developer path is:

- HTTPS bootstrap from one or more public regional edges
- edge discovery through `/gateway/edges`
- health and latency checks via `/gateway/health`
- authenticated HTTPS requests against the selected edge

## Control plane

The management dashboard should stay separate from node hosts.

Recommended services:

- `shardd-dashboard`: auth, operator login, admin UI, developer API key management, and bucket scopes
- future shardd control-plane API additions: bucket inspection, bucket stats, cluster status

The shardd control plane is the correct place for developer/key/scope management.
Bucket inspection should be added as a shardd-specific control-plane surface,
not mixed into node runtime APIs.
