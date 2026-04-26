# Introduction

`shardd` is a globally distributed credit ledger for metered billing.
Credit, debit, and hold accounts from any region — no central
database, no leader, no consensus tax. Edge gateways in `use1`,
`euc1`, and `ape1` accept writes locally and replicate asynchronously
across a private libp2p mesh.

## What you'll do

The whole API surface is essentially three operations:

- `POST /events` — credit, debit, or hold a balance. Idempotent on a caller-supplied nonce.
- `GET /balances?bucket=<scope>` — read every account's balance in a bucket.
- `GET /collapsed/<bucket>/<account>` — read one account's balance and active holds.

Authenticate with `Authorization: Bearer $SHARDD_API_KEY`. Get a key
at [app.shardd.xyz/dashboard/keys](https://app.shardd.xyz/dashboard/keys).

## Mental model

- A **bucket** is your scope: `"orders"`, `"team:acme"`, `"workspace:42"`. Pick one per logical product.
- An **account** within a bucket is whatever holds balance: `"user:alice"`, `"invoice:9821"`, `"key:abc123"`.
- Every event is replicated across the cluster within ~50ms; reads are read-your-writes within a bucket.
- Edges fail over automatically — a single regional outage does not affect availability.

## Where to go next

- [**Quickstart**](/guide/quickstart) — copy-pasteable curl, Rust, Python, TypeScript, Kotlin examples for every operation. Pick a tab; the choice sticks across pages.
- [**SDKs**](/guide/sdks) — package coordinates for the four first-party clients (`shardd` on crates.io / PyPI, `@shardd/sdk` on npm, `xyz.shardd:sdk` on Maven Central) and what every SDK does for free (probe, rank, fail over, idempotency).
- [**CLI**](/guide/cli) — `cargo install shardd-cli` for a terminal-based front-end with browser-based device-flow auth. Same operations as the dashboard.
- [**AI Agent Prompt**](/guide/ai-agent-setup) — drop-in prompt for Claude Code / Cursor / Codex that wires shardd into whatever language and tooling your project already uses.
- [**Public Edge Clients**](/guide/public-edge-clients) — when you're not on a supported language and need to talk HTTPS directly.
- [**Architecture**](/guide/architecture) — how the private mesh and public edge planes are split.

## For LLMs and AI agents

These docs follow the [llmstxt.org](https://llmstxt.org) convention so AI agents can pull them in one fetch:

- [`/llms.txt`](/llms.txt) — index, one line per page.
- [`/llms-full.txt`](/llms-full.txt) — every guide concatenated as raw markdown. Drop into an LLM context window.

Every guide is also served as raw markdown — append `.md` to any
`/guide/<slug>` URL (e.g. [`/guide/quickstart.md`](/guide/quickstart.md)),
or use the **copy as markdown** icon in the top-right of any guide.
