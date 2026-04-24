# Agent Guide

This repo uses `./run` as the operator entrypoint. Prefer it over calling lower-level scripts unless you are debugging the scripts themselves.

## Working Rules

- Use `rg` / `rg --files` first for searches.
- Do not print secrets. Files under `infra/secrets/` are local-only and ignored.
- Do not revert user changes unless explicitly asked.
- Keep edits scoped to the requested behavior and existing repo patterns.

## Common Checks

```bash
./run fmt
./run lint
./run test
cargo check -p shardd-dashboard
cargo check -p shardd-dashboard-ui
```

## Dashboard UI (Dioxus)

The dashboard frontend is a Dioxus 0.7 WASM app in `apps/dashboard-ui/`. Build it with:

```bash
./run build:ui
```

This runs Tailwind, then `dx bundle --web --release` (the official Dioxus 0.7 production recipe), then wipes and refreshes `apps/dashboard/assets/` with the bundle output. Caddy serves those files at the edge via a bind mount — they are not embedded in the dashboard binary.

## Deployment

Full prod deploy (fmt + lint + test + UI bundle + image builds + infractl apply):

```bash
./run deploy
```

Skip the fmt/lint/test gate when you need to push fast:

```bash
./run deploy:fast
```

Both assume the `prod` deployment. Subcommands and explicit flags still pass through to infractl:

```bash
./run deploy status --deployment prod
./run deploy plan --deployment prod
./run deploy --deployment prod --name shardd-prod-use1-dashboard --skip-build
```

Deploy env vars (`JWT_SECRET`, `BILLING_INTERNAL_SECRET`, etc.) are resolved by `infractl.py` from the deployment's `secret_env` mapping plus `infra/secrets/prod.env`.

### Image distribution (tailnet registry)

A private `registry:2` runs on the dashboard host bound to its tailscale IP
(`100.104.178.26:5000` for prod). `./run deploy` builds images locally,
tags them with the short git SHA (+ `-dirty` if the tree is dirty),
`docker push`es to the registry, and every target host then `docker pull`s
over the tailnet. No more `docker save` + rsync of multi-hundred-MB tars.

Relevant `deploy apply` flags:
- `--transport=registry` (default) or `tar` (break-glass when registry is down)
- `--image-tag <ref>` — force a specific version (rollback)
- `--skip-push` — trust the tag is already in the registry (pairs with `--image-tag` for rollbacks)
- `--skip-build` — reuse local images (still pushes unless `--skip-push`)

Rollback to a prior build:
```bash
./run deploy --deployment prod --image-tag <old-sha> --skip-build --skip-push
```

The operator's laptop must trust the registry for push to work. Add
`/etc/docker/daemon.json` with `{"insecure-registries":["100.104.178.26:5000"]}`
and restart docker once per workstation (tailnet encrypts the traffic end-to-end,
so HTTP is fine).

### Host setup

`./run servers setup --deployment prod` is idempotent and self-healing. It
installs Docker + UFW, enrolls the host in tailnet (`TAILSCALE_AUTH_KEY` from
`prod.env`) with hostname = cluster machine name, writes
`/etc/docker/daemon.json` so the host trusts the tailnet registry, and extends
UFW's `DOCKER-USER` chain to allow `100.64.0.0/10` (tailnet) as a source. Probe
flags gate `fully_setup` — a missing flag blocks `deploy apply` until re-setup.

### Admin UI — mesh inspector

`/admin/mesh` (admin-only) lists each edge gateway and the mesh nodes it sees,
collapsed by default. Each node shows the address libp2p most likely dials over
(`private` / `tailscale` / `public` / `dns`); expand to see every advertised
multiaddr. The gateway's `best_node()` is marked per edge.
