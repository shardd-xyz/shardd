# shardd CLI

`shardd-cli` is the official command-line client. It speaks the same
HTTPS endpoints the dashboard does, so any operation you can perform
in the browser you can script from a terminal — credit/debit, balance
checks, bucket lifecycle, key management, billing, profile.

## Install

```bash
cargo install shardd-cli
```

The binary lands at `~/.cargo/bin/shardd`.

## Log in

```bash
shardd auth login
```

This opens `https://app.shardd.xyz/cli-authorize?session=…` in your
browser. Hit **Authorize**, copy the 10-character verification code,
paste it back into the waiting CLI. The CLI mints a developer API key
for the host and stashes it at `~/.config/shardd/credentials.toml`
(0600).

The key shows in [the dashboard's Keys page](https://app.shardd.xyz/dashboard/keys)
named `cli/<your-hostname>` — revoke or rotate it the same way you
would any other key.

## Common commands

```bash
shardd auth whoami                       # who am I logged in as?
shardd events create --bucket orders --account alice --amount 100
shardd events list   --bucket orders --account alice --limit 50
shardd balances list --bucket orders     # every account in the bucket
shardd accounts get  --bucket orders --account alice
shardd buckets list                      # paginated, supports --status
shardd buckets create --name new-bucket
shardd buckets archive --name old-bucket
shardd keys list
shardd keys create --name "ci-prod" --scope all:rw
shardd profile get
shardd billing status
shardd edges                             # current regional edge directory
shardd health --edge https://use1.api.shardd.xyz
```

Every command returns pretty-printed JSON on stdout. Pipe to `jq` for
filtering and scripting.

## Environment overrides

- `SHARDD_DASHBOARD_URL` — point the CLI at a different dashboard
  (e.g. `http://localhost:8080` for local dev). Set it once at login
  time; the URL is then persisted in `credentials.toml` so subsequent
  invocations don't need it.
- `--dashboard-url <url>` — per-command override of the above.

## Logging out

```bash
shardd auth logout            # also revokes the issued key on the server
shardd auth logout --local    # delete local creds only; key stays alive
```

## Source

[github.com/shardd-xyz/shardd/tree/main/apps/developer-cli](https://github.com/shardd-xyz/shardd/tree/main/apps/developer-cli) ·
MIT license · published as `shardd-cli` on
[crates.io](https://crates.io/crates/shardd-cli).
