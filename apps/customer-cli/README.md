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

> **Status**: 0.0.1 is a scaffold. The full subcommand surface (auth,
> events, balances, accounts, buckets, keys, profile, billing, edges,
> health) lands in 0.1.0 — track progress at
> [github.com/shardd-xyz/shardd/issues](https://github.com/shardd-xyz/shardd/issues).

## License

MIT.
