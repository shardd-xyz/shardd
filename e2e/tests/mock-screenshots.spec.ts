import { test } from "@playwright/test";
import { installMockApi, smokeBucket, smokeAccount, smokeNote } from "../support/dashboard-fixtures";

const outDir = process.env.SCREENSHOT_DIR || "/tmp/shardd-mock-screenshots";

test.describe("mock dashboard screenshots", () => {
  test.skip(!process.env.CAPTURE_MOCK_SCREENSHOTS, "Set CAPTURE_MOCK_SCREENSHOTS=1 to capture");
  test.use({ viewport: { width: 1440, height: 900 } });

  test("dashboard home", async ({ page }) => {
    await installMockApi(page);
    await page.goto("/dashboard");
    await page.waitForSelector(".page-title");
    await page.screenshot({ path: `${outDir}/dashboard-home.png`, fullPage: true });
  });

  test("dashboard home frozen", async ({ page }) => {
    await installMockApi(page);
    await page.route("**/api/developer/me", (route) => {
      route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({ is_frozen: true }),
      });
    });
    await page.goto("/dashboard");
    await page.waitForSelector(".banner-warning");
    await page.screenshot({ path: `${outDir}/dashboard-home-frozen.png`, fullPage: true });
  });

  test("bucket explorer table", async ({ page }) => {
    await installMockApi(page);
    await page.goto("/dashboard/buckets");
    await page.waitForSelector("table tbody tr");
    await page.screenshot({ path: `${outDir}/bucket-explorer.png`, fullPage: true });
  });

  test("keys list condensed", async ({ page }) => {
    await installMockApi(page);
    await page.route("**/api/developer/keys", (route) => {
      if (route.request().method() !== "GET") return route.fallback();
      route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify([
          {
            id: "key_01",
            name: "production-worker",
            key_prefix: "sk_live_d8h2",
            last_used_at: new Date(Date.now() - 1000 * 60 * 4).toISOString(),
            created_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 12).toISOString(),
            expires_at: null,
            revoked_at: null,
          },
          {
            id: "key_02",
            name: "ci-runner",
            key_prefix: "sk_live_qz9a",
            last_used_at: new Date(Date.now() - 1000 * 60 * 60 * 5).toISOString(),
            created_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 30).toISOString(),
            expires_at: new Date(Date.now() + 1000 * 60 * 60 * 24 * 14).toISOString(),
            revoked_at: null,
          },
          {
            id: "key_03",
            name: "old-deploy",
            key_prefix: "sk_live_a1b2",
            last_used_at: null,
            created_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 90).toISOString(),
            expires_at: null,
            revoked_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 7).toISOString(),
          },
        ]),
      });
    });
    await page.goto("/dashboard/keys");
    await page.waitForSelector(".key-card");
    await page.screenshot({ path: `${outDir}/keys-list.png`, fullPage: true });
  });

  test("admin audit truncation", async ({ page }) => {
    await installMockApi(page, { session: "admin" });
    await page.route("**/api/admin/audit**", (route) => {
      route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          total: 3,
          entries: [
            {
              created_at: new Date().toISOString(),
              admin_email: "ops@shardd.xyz",
              action: "user.impersonate",
              target_email: "very-long-name-that-should-truncate@longdomain.example.com",
              target_user_id: "33333333-3333-4333-8333-333333333333",
              metadata: { reason: "support escalation" },
            },
            {
              created_at: new Date(Date.now() - 1000 * 60 * 60).toISOString(),
              admin_email: "ops@shardd.xyz",
              action: "user.freeze",
              target_email: null,
              target_user_id: "ffffffff-ffff-4fff-bfff-ffffffffffff",
              metadata: {},
            },
            {
              created_at: new Date(Date.now() - 1000 * 60 * 60 * 6).toISOString(),
              admin_email: "ops@shardd.xyz",
              action: "user.unfreeze",
              target_email: "alice@shardd.xyz",
              target_user_id: "11111111-1111-4111-8111-111111111111",
              metadata: {},
            },
          ],
        }),
      });
    });
    await page.goto("/admin/audit");
    await page.waitForSelector(".audit-target");
    await page.screenshot({ path: `${outDir}/admin-audit.png`, fullPage: true });
  });

  test("dashboard home narrow viewport", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await installMockApi(page, { session: "admin" });
    await page.goto("/dashboard");
    await page.waitForSelector(".page-title");
    await page.screenshot({ path: `${outDir}/mobile-dashboard-home.png`, fullPage: true });
  });

  test("account detail with active holds", async ({ page }) => {
    await installMockApi(page);
    const now = Date.now();
    await page.route("**/api/developer/buckets/**/events**", (route) => {
      route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          total: 4,
          events: [
            {
              created_at_unix_ms: now - 1000 * 60 * 2,
              event_id: "evt_hold_alpha",
              idempotency_nonce: "auth-7731",
              origin_node_id: "12D3KooWAlpha8h2zNpQrLkRtYxMv1",
              account: smokeAccount,
              type: "reservation_create",
              amount: -75,
              hold_amount: 75,
              hold_expires_at_unix_ms: now + 1000 * 60 * 12,
              note: "Authorization hold for in-flight order #7731",
            },
            {
              created_at_unix_ms: now - 1000 * 60 * 25,
              event_id: "evt_hold_beta",
              idempotency_nonce: "auth-9120",
              origin_node_id: "12D3KooWBeta3kP9LzQjRsTuVwXy2",
              account: smokeAccount,
              type: "reservation_create",
              amount: -150,
              hold_amount: 150,
              hold_expires_at_unix_ms: now + 1000 * 60 * 45,
              note: "Subscription renewal hold",
            },
            {
              created_at_unix_ms: now - 1000 * 60 * 40,
              event_id: "evt_hold_gamma",
              idempotency_nonce: "auth-soon",
              origin_node_id: "12D3KooWAlpha8h2zNpQrLkRtYxMv1",
              account: smokeAccount,
              type: "reservation_create",
              amount: -25,
              hold_amount: 25,
              hold_expires_at_unix_ms: now + 1000 * 30,
              note: "Quick auth about to expire",
            },
            {
              created_at_unix_ms: now - 1000 * 60 * 60 * 3,
              event_id: "evt_settled",
              idempotency_nonce: "settle-alpha",
              origin_node_id: "12D3KooWAlpha8h2zNpQrLkRtYxMv1",
              account: smokeAccount,
              type: "standard",
              amount: 1000,
              hold_amount: 0,
              hold_expires_at_unix_ms: 0,
              note: "Settled deposit",
            },
          ],
        }),
      });
    });
    await page.route("**/api/developer/buckets/**", (route) => {
      if (!route.request().url().endsWith(smokeBucket)) return route.fallback();
      route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          summary: {
            bucket: smokeBucket,
            account_count: 1,
            event_count: 4,
            available_balance: 750,
            active_hold_total: 250,
            last_event_at_unix_ms: now - 1000 * 60 * 2,
          },
          accounts: [{
            account: smokeAccount,
            balance: 1000,
            available_balance: 750,
            active_hold_total: 250,
            event_count: 4,
            last_event_at_unix_ms: now - 1000 * 60 * 2,
          }],
        }),
      });
    });
    await page.goto(`/dashboard/buckets/${smokeBucket}/accounts/${smokeAccount}`);
    await page.waitForSelector(".event-list .event-card");
    await page.screenshot({ path: `${outDir}/account-detail.png`, fullPage: true });
  });

  test("bucket detail events with long note", async ({ page }) => {
    await installMockApi(page);
    await page.route("**/api/developer/buckets/**/events**", (route) => {
      route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          total: 2,
          events: [
            {
              created_at_unix_ms: Date.now() - 60_000,
              event_id: "evt_long_note",
              idempotency_nonce: "support-escalation-2026",
              account: smokeAccount,
              type: "standard",
              amount: -500,
              note: "Customer hit a stuck checkout flow on web after the 4/15 deploy and we manually refunded the failed authorization. Logs show the gateway timed out twice before we caught the partial debit. Followups: re-run reconciliation against the gateway export, ping infra about the 504s on the auth path, and confirm with the customer that the refund settled before EOD. This note is intentionally long to exercise the 3-line clamp.",
            },
            {
              created_at_unix_ms: Date.now() - 1000 * 60 * 30,
              event_id: "evt_short_note",
              idempotency_nonce: null,
              account: smokeAccount,
              type: "standard",
              amount: 25,
              note: "Routine top-up.",
            },
          ],
        }),
      });
    });
    await page.goto(`/dashboard/buckets/${smokeBucket}`);
    await page.waitForSelector(".event-list .event-card-note");
    await page.screenshot({ path: `${outDir}/bucket-detail-long-note.png`, fullPage: true });
    await page.locator(".event-card-note").first().click();
    await page.waitForSelector(".event-card-note.is-expanded");
    await page.screenshot({ path: `${outDir}/bucket-detail-long-note-expanded.png`, fullPage: true });
  });

  test("bucket detail events cards", async ({ page }) => {
    await installMockApi(page);
    await page.route("**/api/developer/buckets/**/events**", (route) => {
      route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          total: 5,
          events: [
            {
              created_at_unix_ms: Date.now() - 45_000,
              event_id: "evt_01HV7Q8YPC",
              idempotency_nonce: "shopify-order-9821",
              account: smokeAccount,
              type: "standard",
              amount: 1250,
              note: "Shopify checkout #9821 credit",
            },
            {
              created_at_unix_ms: Date.now() - 1000 * 60 * 8,
              event_id: "evt_01HV7P32AB",
              idempotency_nonce: "refund-9821",
              account: smokeAccount,
              type: "void",
              amount: -420,
              note: "Partial refund for damaged line item",
            },
            {
              created_at_unix_ms: Date.now() - 1000 * 60 * 55,
              event_id: "evt_01HV7MKWQ0",
              idempotency_nonce: "hold-auth-7731",
              account: "reserved",
              type: "reservation_create",
              amount: -75,
              note: "Authorization hold for in-flight order",
            },
            {
              created_at_unix_ms: Date.now() - 1000 * 60 * 60 * 6,
              event_id: "evt_01HV7J11XX",
              idempotency_nonce: null,
              account: smokeAccount,
              type: "hold_release",
              amount: 75,
              note: null,
            },
            {
              created_at_unix_ms: Date.now() - 1000 * 60 * 60 * 48,
              event_id: "evt_01HV750QR9",
              idempotency_nonce: "nightly-sweep-20260414",
              account: "payout",
              type: "standard",
              amount: -9200,
              note: smokeNote,
            },
          ],
        }),
      });
    });

    await page.goto(`/dashboard/buckets/${smokeBucket}`);
    await page.waitForSelector(".event-list .event-card");
    await page.screenshot({ path: `${outDir}/bucket-detail-events.png`, fullPage: true });
  });
});
