// Self-contained dashboard screenshot driver for the Product Hunt
// launch. Boots the mock static-asset server, intercepts every
// /api/* call with a rich demo dataset (3 buckets, several accounts,
// dozens of varied events, three API keys including a CLI control
// key), then walks the dashboard pages and writes PNGs to
// /tmp/shardd-marketing/.
//
// Doesn't use the existing playwright spec because that one relies on
// vanilla-JS class selectors (.page-title, .key-card, etc.) that
// don't exist in the current Dioxus bundle. This script uses
// networkidle + a small settle delay instead.
//
// Usage: cd e2e && node marketing-screenshots.mjs
//   - starts mock asset server on 127.0.0.1:$PLAYWRIGHT_MOCK_PORT (default 4183)
//   - spawns chromium
//   - shoots ~10 PNGs
//   - cleans up

import { chromium } from "playwright";
import { mkdir, writeFile, stat, readFile } from "node:fs/promises";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const SELF_DIR = dirname(fileURLToPath(import.meta.url));
const OUT_DIR = process.env.SCREENSHOT_DIR || "/tmp/shardd-marketing";
const PORT = Number(process.env.PLAYWRIGHT_MOCK_PORT || 4183);
const BASE_URL = `http://127.0.0.1:${PORT}`;
const VIEWPORT = { width: 1440, height: 900 };
const SETTLE_MS = 700;

await mkdir(OUT_DIR, { recursive: true });

// ── demo data ────────────────────────────────────────────────────────

const NOW = Date.UTC(2026, 3, 25, 16, 30, 0); // 2026-04-25 16:30 UTC

const DEMO_USER = {
  id: "11111111-1111-4111-8111-111111111111",
  email: "alice@acme.test",
  display_name: "Alice (Acme Robotics)",
  language: "en",
  is_admin: false,
  is_frozen: false,
  created_at: new Date(NOW - 1000 * 60 * 60 * 24 * 47).toISOString(),
  updated_at: new Date(NOW - 1000 * 60 * 60 * 4).toISOString(),
  last_login_at: new Date(NOW - 1000 * 60 * 22).toISOString(),
};

const BUCKETS = [
  {
    bucket: "api-credits",
    status: "active",
    total_balance: 124_500,
    available_balance: 120_300,
    account_count: 8,
    event_count: 1842,
    last_event_at_unix_ms: NOW - 1000 * 60 * 2,
    created_at_unix_ms: NOW - 1000 * 60 * 60 * 24 * 47,
  },
  {
    bucket: "team-quotas",
    status: "active",
    total_balance: 38_200,
    available_balance: 35_800,
    account_count: 24,
    event_count: 612,
    last_event_at_unix_ms: NOW - 1000 * 60 * 18,
    created_at_unix_ms: NOW - 1000 * 60 * 60 * 24 * 12,
  },
  {
    bucket: "customer-balances",
    status: "active",
    total_balance: 1_204_000,
    available_balance: 1_198_750,
    account_count: 312,
    event_count: 8430,
    last_event_at_unix_ms: NOW - 1000 * 30,
    created_at_unix_ms: NOW - 1000 * 60 * 60 * 24 * 89,
  },
  {
    bucket: "ops-internal",
    status: "archived",
    total_balance: 0,
    available_balance: 0,
    account_count: 3,
    event_count: 84,
    last_event_at_unix_ms: NOW - 1000 * 60 * 60 * 24 * 60,
    created_at_unix_ms: NOW - 1000 * 60 * 60 * 24 * 180,
    archived_at_unix_ms: NOW - 1000 * 60 * 60 * 24 * 30,
  },
];

const ACCOUNTS = {
  "api-credits": [
    { account: "user:01", balance: 18_500, available_balance: 18_500, active_hold_total: 0, event_count: 412, last_event_at_unix_ms: NOW - 1000 * 60 * 2 },
    { account: "user:02", balance: 21_400, available_balance: 19_900, active_hold_total: 1500, event_count: 388, last_event_at_unix_ms: NOW - 1000 * 60 * 9 },
    { account: "user:03", balance: 9_750, available_balance: 9_000, active_hold_total: 750, event_count: 226, last_event_at_unix_ms: NOW - 1000 * 60 * 14 },
    { account: "user:04", balance: 32_600, available_balance: 31_400, active_hold_total: 1200, event_count: 287, last_event_at_unix_ms: NOW - 1000 * 60 * 33 },
    { account: "user:05", balance: 6_900, available_balance: 6_700, active_hold_total: 200, event_count: 145, last_event_at_unix_ms: NOW - 1000 * 60 * 47 },
    { account: "user:06", balance: 15_350, available_balance: 15_000, active_hold_total: 350, event_count: 198, last_event_at_unix_ms: NOW - 1000 * 60 * 95 },
    { account: "user:07", balance: 12_000, available_balance: 12_000, active_hold_total: 0, event_count: 96, last_event_at_unix_ms: NOW - 1000 * 60 * 60 * 2 },
    { account: "user:08", balance: 8_000, available_balance: 7_600, active_hold_total: 400, event_count: 90, last_event_at_unix_ms: NOW - 1000 * 60 * 60 * 4 },
  ],
  "team-quotas": [
    { account: "team:platform", balance: 22_000, available_balance: 22_000, active_hold_total: 0, event_count: 312, last_event_at_unix_ms: NOW - 1000 * 60 * 18 },
    { account: "team:growth", balance: 8_500, available_balance: 6_400, active_hold_total: 2100, event_count: 187, last_event_at_unix_ms: NOW - 1000 * 60 * 41 },
    { account: "team:research", balance: 7_700, available_balance: 7_400, active_hold_total: 300, event_count: 113, last_event_at_unix_ms: NOW - 1000 * 60 * 60 * 3 },
  ],
  "customer-balances": [
    { account: "cust:0001", balance: 24_000, available_balance: 24_000, active_hold_total: 0, event_count: 84, last_event_at_unix_ms: NOW - 1000 * 30 },
    { account: "cust:0002", balance: 9_500, available_balance: 9_500, active_hold_total: 0, event_count: 41, last_event_at_unix_ms: NOW - 1000 * 60 * 5 },
    { account: "cust:0003", balance: 132_000, available_balance: 130_000, active_hold_total: 2000, event_count: 271, last_event_at_unix_ms: NOW - 1000 * 60 * 12 },
  ],
};

const NOTES = [
  "Stripe charge succeeded · invoice in_1Pq8x2", "API call /v1/completions × 42",
  "Manual top-up via support ticket #2031", "Subscription renewal — Plus plan",
  "Hold released · order shipped", "Refund issued for cancelled order",
  "Daily LLM quota grant", "Edge cache miss — rerun",
  "Auth hold for in-flight order", "Background reconciliation sweep",
  "Replenish from prepaid pool", "Trial credit grant", "Annual upgrade prorated credit",
  "Manual debit · disputed charge", "Bonus credit for early-access user",
];

const TYPES = ["standard", "standard", "standard", "standard", "reservation_create", "hold_release", "void"];

function makeEvents(bucket, accountList) {
  const events = [];
  for (let i = 0; i < 24; i++) {
    const acct = accountList[i % accountList.length].account;
    const minutesAgo = i * 8 + Math.floor(Math.random() * 4);
    const sign = i % 5 === 0 ? -1 : 1;
    const amount = sign * (50 + Math.floor(Math.random() * 950));
    const type = TYPES[i % TYPES.length];
    events.push({
      created_at_unix_ms: NOW - 1000 * 60 * minutesAgo,
      event_id: `evt_${bucket.replace(/[^a-z]/g, "").slice(0, 4)}_${i.toString().padStart(4, "0")}`,
      idempotency_nonce: `${bucket.split(/-/)[0]}-${1000 + i}`,
      origin_node_id:
        i % 3 === 0
          ? "12D3KooWUseastNode"
          : i % 3 === 1
            ? "12D3KooWEucentralNode"
            : "12D3KooWApsouthNode",
      origin_epoch: 1,
      origin_seq: 100_000 + i,
      account: acct,
      type,
      amount,
      hold_amount: type === "reservation_create" ? Math.abs(amount) : 0,
      hold_expires_at_unix_ms:
        type === "reservation_create"
          ? NOW + 1000 * 60 * (10 + (i % 30))
          : 0,
      note: NOTES[i % NOTES.length],
    });
  }
  return events;
}

const KEYS = [
  {
    id: "k1",
    user_id: DEMO_USER.id,
    name: "production-worker",
    key_prefix: "sk_live_d8h2K3aA9R0w",
    last_used_at: new Date(NOW - 1000 * 60 * 4).toISOString(),
    created_at: new Date(NOW - 1000 * 60 * 60 * 24 * 12).toISOString(),
    updated_at: new Date(NOW - 1000 * 60 * 60 * 24 * 12).toISOString(),
    expires_at: null,
    revoked_at: null,
  },
  {
    id: "k2",
    user_id: DEMO_USER.id,
    name: "ci-runner",
    key_prefix: "sk_live_qz9aBn4Lm2p7",
    last_used_at: new Date(NOW - 1000 * 60 * 60 * 5).toISOString(),
    created_at: new Date(NOW - 1000 * 60 * 60 * 24 * 30).toISOString(),
    updated_at: new Date(NOW - 1000 * 60 * 60 * 24 * 30).toISOString(),
    expires_at: new Date(NOW + 1000 * 60 * 60 * 24 * 14).toISOString(),
    revoked_at: null,
  },
  {
    id: "k3",
    user_id: DEMO_USER.id,
    name: "cli/alice-laptop/qDvqWo",
    key_prefix: "sk_live_eR8nC2v4Bz0K",
    last_used_at: new Date(NOW - 1000 * 60 * 22).toISOString(),
    created_at: new Date(NOW - 1000 * 60 * 60 * 3).toISOString(),
    updated_at: new Date(NOW - 1000 * 60 * 60 * 3).toISOString(),
    expires_at: null,
    revoked_at: null,
  },
];

const SCOPES = {
  k1: [
    { id: "s1", api_key_id: "k1", resource_type: "bucket", match_type: "all", resource_value: null, can_read: true, can_write: true, created_at: new Date(NOW - 1000 * 60 * 60 * 24 * 12).toISOString() },
  ],
  k2: [
    { id: "s2", api_key_id: "k2", resource_type: "bucket", match_type: "exact", resource_value: "api-credits", can_read: true, can_write: false, created_at: new Date(NOW - 1000 * 60 * 60 * 24 * 30).toISOString() },
  ],
  k3: [
    { id: "s3a", api_key_id: "k3", resource_type: "bucket", match_type: "all", resource_value: null, can_read: true, can_write: true, created_at: new Date(NOW - 1000 * 60 * 60 * 3).toISOString() },
    { id: "s3b", api_key_id: "k3", resource_type: "control", match_type: "all", resource_value: null, can_read: true, can_write: true, created_at: new Date(NOW - 1000 * 60 * 60 * 3).toISOString() },
  ],
};

const EDGES = [
  { edge_id: "use1", region: "us-east-1", base_url: "https://use1.api.shardd.xyz", node_id: "73119020", label: "aws-use1", node_label: "aws-use1-mesh" },
  { edge_id: "euc1", region: "eu-central-1", base_url: "https://euc1.api.shardd.xyz", node_id: "67139d1d", label: "aws-euc1", node_label: "aws-euc1-mesh" },
  { edge_id: "ape1", region: "ap-east-1", base_url: "https://ape1.api.shardd.xyz", node_id: "899f82a2", label: "aws-ape1", node_label: "aws-ape1-mesh" },
];

const BILLING_STATUS = {
  plan_slug: "scale",
  plan_name: "Scale",
  subscription_status: "active",
  monthly_credits: 1_000_000,
  credit_balance: 712_400,
  period_start: new Date(NOW - 1000 * 60 * 60 * 24 * 8).toISOString(),
  period_end: new Date(NOW + 1000 * 60 * 60 * 24 * 22).toISOString(),
};

const BILLING_PLANS = [
  { slug: "starter", name: "Starter", monthly_credits: 50_000, price_cents: 0, annual_price_cents: 0 },
  { slug: "team", name: "Team", monthly_credits: 250_000, price_cents: 4900, annual_price_cents: 52900 },
  { slug: "scale", name: "Scale", monthly_credits: 1_000_000, price_cents: 19900, annual_price_cents: 214900 },
];

// ── /api router ──────────────────────────────────────────────────────

function bucketDetail(name) {
  const b = BUCKETS.find((x) => x.bucket === name);
  if (!b) return null;
  const accounts = ACCOUNTS[name] || [];
  const active_hold_total = accounts.reduce(
    (sum, a) => sum + (a.active_hold_total || 0),
    0,
  );
  return {
    summary: {
      bucket: b.bucket,
      account_count: b.account_count,
      event_count: b.event_count,
      available_balance: b.available_balance,
      active_hold_total,
      last_event_at_unix_ms: b.last_event_at_unix_ms,
    },
    accounts,
  };
}

function bucketEventList(name) {
  const list = makeEvents(name, ACCOUNTS[name] || [{ account: "main" }]);
  return { total: list.length, events: list, page: 1, limit: list.length };
}

function eventsAcrossBuckets() {
  const all = [];
  for (const b of BUCKETS.filter((x) => x.status === "active")) {
    for (const e of makeEvents(b.bucket, ACCOUNTS[b.bucket] || [])) {
      all.push({ ...e, bucket: b.bucket });
    }
  }
  all.sort((a, b) => b.created_at_unix_ms - a.created_at_unix_ms);
  return {
    total: all.length,
    limit: 50,
    offset: 0,
    events: all.slice(0, 50),
    heads: {},
    max_known_seqs: {},
  };
}

async function installRoutes(page) {
  await page.route("**/api/**", async (route) => {
    const req = route.request();
    const u = new URL(req.url());
    const m = req.method();
    const json = (b, s = 200) =>
      route.fulfill({ status: s, contentType: "application/json", body: JSON.stringify(b) });

    if (u.pathname === "/api/auth/verify" && m === "GET") {
      return json({ id: DEMO_USER.id, email: DEMO_USER.email, is_admin: false });
    }
    if (u.pathname === "/api/developer/me" && m === "GET") {
      return json({ ...DEMO_USER });
    }
    if (u.pathname === "/api/user" && m === "PATCH") return json(DEMO_USER);
    if (u.pathname === "/api/user/export" && m === "GET")
      return json({ user: DEMO_USER, api_keys: KEYS, buckets: BUCKETS });
    if (u.pathname === "/api/developer/keys" && m === "GET") return json(KEYS);
    {
      const mk = u.pathname.match(/^\/api\/developer\/keys\/([^/]+)\/scopes$/);
      if (mk && m === "GET") return json(SCOPES[mk[1]] || []);
    }
    if (u.pathname === "/api/developer/edges" && m === "GET") return json(EDGES);

    if (u.pathname === "/api/developer/buckets" && m === "GET") {
      const status = u.searchParams.get("status") || "active";
      const wanted =
        status === "all"
          ? BUCKETS
          : BUCKETS.filter((b) => b.status === status);
      return json({ buckets: wanted, total: wanted.length, page: 1, limit: wanted.length });
    }
    {
      const mb = u.pathname.match(/^\/api\/developer\/buckets\/([^/]+)$/);
      if (mb && m === "GET") {
        const detail = bucketDetail(mb[1]);
        if (!detail) return json({ code: "NOT_FOUND" }, 404);
        return json(detail);
      }
    }
    {
      const me = u.pathname.match(/^\/api\/developer\/buckets\/([^/]+)\/events$/);
      if (me && m === "GET") return json(bucketEventList(me[1]));
    }
    if (u.pathname === "/api/developer/events" && m === "GET")
      return json(eventsAcrossBuckets());

    if (u.pathname === "/api/billing/status" && m === "GET") return json(BILLING_STATUS);
    if (u.pathname === "/api/billing/plans" && m === "GET") return json(BILLING_PLANS);

    return json({ code: "NOT_IMPLEMENTED", path: u.pathname }, 404);
  });
}

// ── mock-server boot ─────────────────────────────────────────────────

const mockServer = spawn("node", [join(SELF_DIR, "support/mock-server.mjs")], {
  stdio: ["ignore", "pipe", "pipe"],
  env: { ...process.env, PLAYWRIGHT_MOCK_PORT: String(PORT) },
});

await new Promise((resolve, reject) => {
  const onErr = (d) => process.stderr.write(d);
  const onOut = (d) => {
    const s = d.toString();
    if (s.includes("listening on")) {
      mockServer.stdout.off("data", onOut);
      resolve();
    }
  };
  mockServer.stdout.on("data", onOut);
  mockServer.stderr.on("data", onErr);
  setTimeout(() => reject(new Error("mock server didn't start in 5s")), 5000);
});

// ── shoot ────────────────────────────────────────────────────────────

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: VIEWPORT });
const page = await ctx.newPage();
await installRoutes(page);

const targets = [
  { name: "01-dashboard-home", path: "/dashboard" },
  { name: "02-buckets-list", path: "/dashboard/buckets" },
  { name: "03-bucket-detail-api-credits", path: "/dashboard/buckets/api-credits" },
  { name: "04-account-detail", path: "/dashboard/buckets/api-credits/accounts/user:02" },
  { name: "05-events-feed", path: "/dashboard/events" },
  { name: "06-keys-list", path: "/dashboard/keys" },
  { name: "07-billing", path: "/dashboard/billing" },
  { name: "08-profile", path: "/profile" },
  { name: "09-bucket-detail-customers", path: "/dashboard/buckets/customer-balances" },
];

for (const t of targets) {
  const url = `${BASE_URL}${t.path}`;
  try {
    const resp = await page.goto(url, { waitUntil: "networkidle", timeout: 15_000 });
    await page.waitForTimeout(SETTLE_MS);
    if (t.name === "06-keys-list") {
      // Expand every <details> on the keys page so the per-key
      // scope rows (including the new "dashboard control" badge for
      // CLI-issued keys) are visible in the screenshot.
      await page
        .locator("details")
        .evaluateAll((nodes) => nodes.forEach((n) => n.setAttribute("open", "")));
      await page.waitForTimeout(200);
    }
    const out = `${OUT_DIR}/${t.name}.png`;
    await page.screenshot({ path: out, fullPage: true });
    const sz = (await stat(out)).size;
    console.log(`OK ${t.name}  status=${resp ? resp.status() : "?"}  bytes=${sz}`);
  } catch (e) {
    console.log(`ERR ${t.name}  ${e.message}`);
  }
}

await browser.close();
mockServer.kill();
console.log(`\nWrote screenshots to ${OUT_DIR}`);
