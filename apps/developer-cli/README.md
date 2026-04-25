# shardd CLI

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Official command-line client for [shardd](https://shardd.xyz) — a globally
distributed credit ledger with automatic regional failover.

This crate publishes the `shardd` binary. After installing, run:

```bash
shardd auth login
shardd events create --bucket orders --account alice --amount 100
shardd balances list --bucket orders
shardd buckets list
shardd keys list
```

## Install

```bash
cargo install shardd-cli
```

The binary lands at `~/.cargo/bin/shardd`.

> **0.1.0** is the first release with the full surface (auth, events,
> balances, accounts, buckets, keys, profile, billing, edges, health).
> Browser device-flow auth lands an API key at
> `~/.config/shardd/credentials.toml`; subsequent commands speak HTTPS
> to the dashboard's `/api/developer/*` and `/api/auth/cli/*` endpoints.
>
> CLI keys are minted with both a data-plane (`bucket:all:rw`) and a
> control-plane (`control:all:rw`) scope. Existing keys created via
> the dashboard before this release have only bucket scopes — they
> still write events but can't manage other keys, archive buckets,
> or hit profile/billing. Re-issue via `shardd auth login` (or tick
> "Allow dashboard control" in the dashboard's create-key wizard).

## License

MIT.
