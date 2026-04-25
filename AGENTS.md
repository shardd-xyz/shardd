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

## Migrations

Files under `libs/storage/migrations/`, `apps/dashboard/migrations/`,
and `apps/billing/migrations/` are loaded by `sqlx::migrate!()` at
service startup. Once a migration has run on a real DB, **never edit
the file in place** — sqlx records a SHA-384 of every applied migration
in `_sqlx_migrations.checksum` and refuses to start with `migration N
was previously applied but has been modified` if the on-disk content
changes. Any post-apply edit needs either:

1. A new forward-only migration file (preferred), or
2. A pre-deploy `UPDATE _sqlx_migrations SET checksum = decode('<new sha384>','hex') WHERE version = N;` against every existing DB — only safe if the edit is genuinely cosmetic (comments / whitespace) and the schema is unchanged.

If you see that error in the field, prod is the source of truth: run
`sha384sum libs/.../00N_*.sql`, patch the checksum row in each affected
DB, and the crashlooping containers will boot on the next restart cycle.

The rule is enforced by two gates:

1. **Pre-commit hook** at `scripts/git-hooks/pre-commit` refuses any
   commit that modifies, renames, or deletes a tracked migration file.
   Adds (new migrations) are allowed. Enable once per workstation:
   ```bash
   ./run hooks:install
   ```
   Bypass deliberately with `GIT_ALLOW_MIGRATION_EDIT=1 git commit ...`
   when the edit is genuinely cosmetic and you commit to patching
   `_sqlx_migrations.checksum` in every existing DB beforehand.

2. **Deploy gate** in `./run deploy` refuses to ship if any file under
   `*/migrations/*.sql` is dirty (modified, staged, or untracked).
   Every migration that lands in a prod image must already be in git
   history so deploys are reproducible from a SHA. The gate fires for
   both `./run deploy` and `./run deploy:fast`.

3. **Drift diagnostic** — `./run migrations:check` SSHes into every
   prod DB host (3 full-node DBs, dashboard, billing), dumps
   `_sqlx_migrations.checksum`, and compares against `sha384sum` of
   the local source files. Read-only — prints a ready-to-paste
   `UPDATE _sqlx_migrations SET checksum = decode('…','hex') WHERE
   version = N;` for each drifted row. Run before every deploy when
   touching migrations, and every time a service crashloops with
   "previously applied but has been modified".

## FULL LOOP (commit → push → deploy → validate)

When the user says "do the FULL LOOP" (or "full loop"), execute these
phases in order. **Don't skip or reorder them**, and stop on any red.

### 1. Mini-loop — local validation gate
Block on any failure; never push or deploy with red.
```bash
./run fmt
./run lint
./run test
./run sdk:test:failover    # 3-gateway docker harness, all 4 SDKs × 2 phases
```
Add `cargo check -p shardd-dashboard-ui` if the change touched Dioxus.

### 2. Push
`landing/` is a git submodule pointing at `shardd-xyz/shardd-landing`.
When it has unpushed commits, push the submodule first so the SHA is
resolvable on the remote, then bump the pointer in the parent repo:
```bash
git -C landing push origin main
git add landing && git commit -m "Bump landing submodule for ..."
git push origin main
```
If only the main repo is dirty, just `git push`.

### 3. Deploy
```bash
./run deploy
```
Re-runs fmt/lint/test, bundles the Dioxus UI, builds + pushes images
to the tailnet registry, applies via infractl. Use `./run deploy:fast`
only if the mini-loop already passed and you trust the gate.

### 4. Prod validation
- **Edge health** — each region's gateway should be ready with ≥2 healthy nodes:
  ```bash
  for r in use1 euc1 ape1; do
    curl -sS "https://$r.api.shardd.xyz/gateway/health" | jq '{edge_id,ready,healthy_nodes,sync_gap}'
  done
  ```
- **HTTP 200** — `curl -sI https://shardd.xyz/` and `https://app.shardd.xyz/` return 200.
- **Screenshots** — capture prod surfaces and read them back to spot regressions:
  ```bash
  cd e2e && node prod-loop-screenshots.mjs
  ```
  This shoots `landing-home`, `landing-quickstart`, `landing-sdks`,
  `landing-public-edge-clients`, and `app-login` into
  `/tmp/prod-screenshots/`. Read each PNG with the Read tool and
  visually confirm the surface you changed actually rendered correctly.
  Authenticated dashboard screenshots use `e2e/dx-screenshot.mjs` (needs
  `RESEND_API_KEY` from `infra/secrets/prod.env`).

### 5. Report
End with: deployed commit SHAs (both repos), prod edge health summary,
which screenshots you visually verified, and any open follow-ups.
