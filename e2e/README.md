# Dashboard E2E Tests

This package contains Playwright coverage for the dashboard SPA.

## Mock UI Tests

```bash
./run e2e:install
./run e2e:mock
```

The mock lane serves `apps/dashboard/assets` and intercepts `/api/*` in the browser. It is intended for PR and local checks.

## Production Smoke

```bash
export RESEND_API_KEY=...
export PLAYWRIGHT_BASE_URL=https://app.shardd.xyz
export PLAYWRIGHT_SMOKE_EMAIL=ops@shardd.xyz
./run e2e:prod
```

The production lane uses the real login form, snapshots Resend received-email IDs, polls for a newly received magic link, opens that link in the same browser context, and then exercises critical dashboard flows. Resend receiving can lag, so the default poll window is 180 seconds; override it with `PLAYWRIGHT_RESEND_TIMEOUT_MS` if needed. The smoke writes only to `codex-smoke-*` buckets and revokes `playwright-smoke-*` API keys it creates.
