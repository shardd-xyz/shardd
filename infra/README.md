# shardd infrastructure

`infra/` is split into three separate control layers:

1. cloud resources
2. host setup
3. service deployment

That split is intentional. Provisioning EC2/DNS/LB resources is not the same
job as preparing hosts, and neither is the same job as rolling out Compose
bundles.

The normal control entrypoint is:

```bash
./run
```

`./infra/infractl.py` still exists underneath, but it is the implementation
detail. Day-to-day operations should go through `./run`.

Runtime services stay Docker/Compose based on the remote hosts. The operator
tooling itself is plain local CLI, not Dockerized.

## Current deployment model

### Full nodes

- `shardd-node`
- local Postgres
- full private libp2p mesh participant
- deployed in core regions

### Edge nodes

- `shardd-gateway`
- public HTTP ingress
- joins the same private mesh with the cluster key
- tracks live mesh health and routes to healthy full nodes
- no local shard state database

### Dashboard host

- shardd-owned control-plane stack
- developer account + API key + bucket scope management
- one host in `us-east-1`

## Why Compose bundles

Three options were reasonable here:

1. one giant hand-written compose file per environment
2. a custom service schema rendered into deployment artifacts
3. reusable role bundles backed by Compose templates

This repo uses option 3.

That means:

- each role has a bundle under `infra/bundles/`
- bundles are simple Compose templates plus optional companion files like `Caddyfile`
- `cluster.json` assigns bundles to machines
- `infractl.py` renders `.env` + bundle files per machine and deploys them

This is simpler than inventing a bespoke orchestrator, and cleaner than keeping
one massive environment-specific Compose file checked in.

## State files

Local state lives under `infra/state/` and is intentionally gitignored.

### `cluster.json`

Desired topology and deployment intent.

It defines:

- deployments such as `prod`
- expected AWS account id for safety checks
- machine names, regions, providers, SSH settings
- which bundle(s) each machine should run
- logical secret env references
- bootstrap/full/edge/dashboard placement

Example:

```bash
cp infra/state/cluster.example.json infra/state/cluster.json
```

### `machines.json`

Observed machine and deployment state.

It records things like:

- provider instance ids
- public/private IPs
- last observed provider state
- setup checks like Docker/UFW/infra SSH key
- whether a host is fully prepared
- last deployed bundle revision per service
- generated per-host secrets such as Postgres passwords

Example:

```bash
cp infra/state/machines.example.json infra/state/machines.json
```

Or initialize both at once:

```bash
./run state init
```

## Current provider support

Terraform now owns the cloud resource layer.

Today that means:

- AWS EC2 instances and security groups
- Cloudflare DNS records, health monitor, pools, and API load balancer

Assumptions in the current Terraform path:

- Ubuntu 24.04 style images
- public IPs on the chosen subnet
- SSH access with the configured EC2 key pair
- local operator machine has `terraform`, `aws`, `ssh`, `rsync`, and `docker`

Additional providers can be added later by extending the Terraform stack and the
`cluster.json -> tfvars` renderer.

## SSH model

There are two SSH layers:

1. **EC2 launch key pair**
- configured per machine in `provider_config.key_name`
- matched locally by `ssh.identity_file`
- used for the first SSH access to a fresh instance

2. **Repo infra key**
- configured by `infra_ssh_public_key_path`
- intended to live under `infra/secrets/`
- installed onto the host during `./run servers setup`

During setup, the tool now installs:

- the dedicated repo infra public key
- every public key found under `~/.ssh/*.pub` on the current operator machine

That means you keep a stable repo-local recovery/admin key, while also allowing
your existing local SSH identities onto the boxes.

Create the repo-local infra key with:

```bash
./run state ensure-ssh-key --deployment prod
```

That is the intended default model:

- one repo-local key under `infra/secrets/`
- one consistent EC2 key pair name such as `shardd-prod-infra`
- the same private key used for initial SSH access in every region

## Machine lifecycle

### 1. Edit the desired cluster

Start from the example and fill in:

- `expected_aws_account_id`
- `dns_root_zone`
- `cloudflare_zone_name`
- `cloudflare_zone_id_env`
- `cloudflare_account_id_env`
- `cloudflare_api_token_env`
- `cloudflare_lb_enabled`
- `cloudflare_lb_monitor`
- `edge_api_dns_name`
- `edge_api_machines`
- `public_edges`
- `infra_ssh_public_key_path`
- `subnet_id`
- `key_name`
- `identity_file`
- optional per-machine `instance_type`
- public DNS/origin values for edge and dashboard hosts

`edge_api_dns_name` is the optional shared public API hostname. `edge_api_machines`
controls which edge hosts participate in the Cloudflare load balancer for that
hostname.

`public_edges` is the public regional edge directory used by HTTPS SDKs. Each
entry should look like:

```json
{
  "edge_id": "use1",
  "region": "us-east-1",
  "base_url": "https://use1.api.dev.example.com"
}
```

Each `edge-node` service then sets `vars.public_edge_id` to one of those
entries. The gateway exposes that information through `/gateway/health` and
`/gateway/edges` for public client bootstrap and failover.

The example prod topology is:

- `us-east-1`: one full node, one edge node, one dashboard host
- `ap-east-1`: one full node, one edge node
- `eu-central-1`: one full node, one edge node

### 2. Export required secrets locally

At minimum:

```bash
export SHARDD_CLUSTER_KEY='replace-with-a-long-random-secret'
export SHARDD_DASHBOARD_MACHINE_AUTH_SECRET='replace-me'
export SHARDD_DASHBOARD_JWT_SECRET='replace-me'
export SHARDD_DASHBOARD_RESEND_API_KEY='replace-me'
export CLOUDFLARE_ZONE_ID='replace-me'
export CLOUDFLARE_ACCOUNT_ID='replace-me'
export CLOUDFLARE_API_TOKEN='replace-me'
```

`cluster.json` points to these environment variable names via `cluster_key_env`
and `secret_env`. Cloudflare env var names are read from the
`cloudflare_*_env` keys in the same deployment block.

Also create the repo-local infra SSH key once:

```bash
./run state ensure-ssh-key --deployment prod
```

### 3. Initialize Terraform

```bash
./run infra:init --deployment prod
```

This renders a deployment-specific `terraform.tfvars.json` under the ignored
`infra/state/terraform/` tree and initializes the local Terraform backend.

### 4. Review the cloud plan

```bash
./run infra:plan --deployment prod
```

### 5. Apply cloud resources

```bash
./run infra:apply --deployment prod
```

That creates or updates:

- EC2 instances
- per-machine security groups
- Cloudflare DNS
- Cloudflare LB monitor, pools, and `api` hostname

It also syncs the current instance IPs and ids back into `machines.json`.

### 6. Prepare the hosts

```bash
./run servers setup --deployment prod
```

### 7. Deploy services

```bash
./run deploy plan --deployment prod
./run deploy --deployment prod
```

### Optional: resync local machine state from Terraform

If Terraform state already exists and you want to repopulate `machines.json`:

```bash
./run infra:output --deployment prod
```

“Fully setup” means:

- Docker installed and active
- UFW installed and active
- Docker/UFW patch applied
- infra SSH public key installed

You can inspect one host directly:

```bash
./run servers inspect --deployment prod --name shardd-prod-use1-full --refresh
```

List the fleet:

```bash
./run servers list --deployment prod
```

Destroy the cloud resources for a deployment:

```bash
./run infra:destroy --deployment prod
```

## Service deployment

Service deployment is separate from host lifecycle.

`./run deploy ...` only works against hosts already present in `machines.json`
and marked `fully_setup=true`.

### Review the plan

```bash
./run deploy plan --deployment prod
```

### Build and apply

```bash
./run deploy --deployment prod
```

Reuse already-built local images:

```bash
./run deploy --deployment prod --skip-build
```

### Show local deployment status

```bash
./run deploy status --deployment prod
```

## Bundles

### `full-node`

Files:

- `infra/bundles/full-node/compose.yml`

Deploys:

- `shardd-node`
- local `postgres:17-alpine`

Opens:

- SSH
- the configured libp2p port

### `edge-node`

Files:

- `infra/bundles/edge-node/compose.yml`
- `infra/bundles/edge-node/Caddyfile`

Deploys:

- `shardd-gateway`
- `caddy`

Opens:

- SSH
- HTTP/HTTPS

### `dashboard`

Files:

- `infra/bundles/dashboard/compose.yml`
- `infra/bundles/dashboard/Caddyfile`

Deploys:

- `shardd-dashboard`
- `postgres`
- `redis`
- `caddy`

Opens:

- SSH
- HTTP/HTTPS

## Image build sources

`deploy apply` builds and ships these local images when needed:

- `local/shardd-node:infra`
- `local/shardd-gateway:infra`
- `local/shardd-dashboard:infra`

## Notes

- Public developers should use HTTPS to edge nodes, not the private node mesh.
- The current public-libp2p SDK story is intentionally deferred. See:
  - [`docs/public-libp2p-clients.md`](/home/user/Workspaces/shardd/docs/public-libp2p-clients.md)
- Node HTTP is not part of production deployment. Full nodes are libp2p only.
