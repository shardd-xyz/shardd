# Drop-in AI agent prompt

Paste this into your AI coding agent (Claude Code, Cursor, Codex, Copilot,
Aider, …) and it'll wire `shardd` into whatever language and tooling
this project already uses. No prior shardd knowledge required from the
agent — the prompt is self-contained.

## The prompt

````text
You are integrating `shardd` (a globally distributed credit ledger with
first-party SDKs for Rust, Python, TypeScript, and Kotlin/JVM) into this
project. Your job: pick the right SDK, install it, wire one shared
client, and add the minimum credit / debit / read call sites — without
changing this project's package manager, framework, or env conventions.

1) Detect the language that owns the code path that needs to write/read
   balances. Inspect the repo root and the directory of the file you'll
   be editing:
   - `Cargo.toml`                                       → Rust;       crate `shardd` on crates.io
   - `pyproject.toml` / `requirements.txt` / `setup.py` → Python;     package `shardd` on PyPI
   - `package.json`                                     → TypeScript; package `@shardd/sdk` on npm
   - `build.gradle.kts` / `build.gradle` / `pom.xml`    → Kotlin/JVM; coordinate `xyz.shardd:sdk:0.1.0` on Maven Central
   If multiple are present, pick the one that owns the new feature.
   Ask if ambiguous. If none of the above (Go, Ruby, PHP, …), fall
   back to plain HTTPS per https://shardd.xyz/guide/public-edge-clients
   and stop reading this prompt.

2) Install with the project's existing tooling — read the lockfile to
   figure out which one. Do not switch package managers.
   - npm / yarn / pnpm / bun: add `@shardd/sdk`
   - pip / poetry / uv / pipenv: add `shardd`
   - `cargo add shardd`. If `tokio` isn't already in `Cargo.toml`, add
     `tokio = { version = "1", features = ["full"] }` — the SDK is async.
   - Gradle: add `implementation("xyz.shardd:sdk:0.1.0")` to dependencies.
     Maven: the equivalent `<dependency>` block.

3) Env. Add `SHARDD_API_KEY` to wherever this project already keeps
   env vars: `.env`, `.env.example`, deployment secrets template, Helm
   values, Vercel/Netlify/Doppler config, whatever applies. Never
   hard-code it in source. The human should grab a key from
   https://app.shardd.xyz/dashboard/keys.

4) Construct the client once and share it. The SDK probes the regional
   edge directory lazily on first use, so reuse one instance per
   process. Stash it in whatever singleton/DI pattern this project
   already uses. The constructor just takes the key:
   - TS:     `new Client(process.env.SHARDD_API_KEY!)`
   - Python: `Shardd(os.environ["SHARDD_API_KEY"])`
   - Rust:   `Client::new(std::env::var("SHARDD_API_KEY")?)?` (sync constructor; methods are async)
   - Kotlin: `Client(System.getenv("SHARDD_API_KEY"))` (blocking)

5) Call sites. These four cover ~everything:
   - Credit / debit / hold: `client.create_event(bucket, account, amount, opts)`
     — positive `amount` = credit, negative = debit. Pass an
     `idempotency_nonce` (TS: `idempotencyNonce`) whenever the call
     lives inside a retry-able pathway so retries collapse onto one
     logical write.
   - All balances in a bucket: `client.get_balances(bucket)`
   - One account's balance + holds: `client.get_account(bucket, account)`
   - Event history of a bucket: `client.list_events(bucket)`
   `bucket` is the project's own scope (e.g. `"orders"`,
   `"team:acme"`); `account` is whatever entity holds balance
   (e.g. `"user:alice"`, `"invoice:9821"`).

6) Do NOT:
   - hand-roll edge probing or retries against `/gateway/health` or
     `/gateway/edges` — the SDK does both, including 60-second
     cool-off and re-probe on `503`/`504`/timeout.
   - pin a single edge URL. The defaults (`use1`, `euc1`, `ape1`)
     rotate automatically by observed latency.
   - generate the idempotency nonce inside the retry loop. Capture
     it above the retry boundary so retries reuse the same nonce.
   - add a custom retry / circuit-breaker library on top — it fights
     the SDK's failover.

7) Verify. Run one write and one read with `SHARDD_API_KEY` set,
   confirm both return non-error. If the project has a test suite,
   add a single integration test gated on `SHARDD_API_KEY` being
   present (skip otherwise so CI without the secret still passes).

References:
- https://shardd.xyz/guide/quickstart    (operations in every language)
- https://shardd.xyz/guide/sdks          (package coordinates + failover behavior)
- https://shardd.xyz/guide/public-edge-clients (raw HTTPS fallback)
````

## What it does

- Detects the project's primary language by inspecting build files (`Cargo.toml`, `pyproject.toml`, `package.json`, `build.gradle.kts`).
- Installs the matching first-party SDK with the project's own package manager.
- Adds `SHARDD_API_KEY` to the project's existing env-var convention.
- Wires a single shared client and sketches the credit/debit/balance call sites.
- Skips manual edge probing and retry plumbing — the SDK already does that.

## Before you run it

1. Grab an API key at [app.shardd.xyz/dashboard/keys](https://app.shardd.xyz/dashboard/keys) and have it ready.
2. Have the project open in your agent's working tree.
3. After it runs, see the [Quickstart](/guide/quickstart) for the full per-language method surface and the [SDKs](/guide/sdks) page for package coordinates and failover details.
