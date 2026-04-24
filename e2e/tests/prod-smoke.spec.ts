import { expect, test } from "@playwright/test";
import { loginWithResendMagicLink } from "../support/prod-email";

test("prod smoke logs in through Resend and verifies critical dashboard flows", async ({ page, request, baseURL }) => {
  const email = process.env.PLAYWRIGHT_SMOKE_EMAIL || "ops@shardd.xyz";
  const bucketPrefix = process.env.PLAYWRIGHT_SMOKE_BUCKET_PREFIX || "codex-smoke";
  const stamp = new Date().toISOString().replace(/[-:.TZ]/g, "").slice(0, 14);
  const bucket = `${bucketPrefix}-${stamp}`;
  const note = `playwright prod smoke ${stamp}`;
  const account = "main";
  const origin = new URL(baseURL || "https://app.shardd.xyz").origin;
  const keyName = `playwright-smoke-${stamp}`;

  await loginWithResendMagicLink(page, request, email, baseURL);
  await expect(page.getByRole("heading", { name: /Overview|Developer home/ })).toBeVisible();

  try {
    await page.goto("/dashboard/keys");
    await page.getByLabel("Key name").fill(keyName);
    await page.getByRole("button", { name: "Create key" }).click();
    await expect(page.getByText("API key created")).toBeVisible();
    await dismissFlash(page);
    await assertNoFailure(page);
    const keyCard = page.locator(".key-card").filter({ hasText: keyName }).first();
    page.once("dialog", (dialog) => dialog.accept());
    await keyCard.locator('button[data-action="revoke-key"]:not([disabled])').click();
    await expect(page.getByText("API key revoked")).toBeVisible();
    await assertNoFailure(page);

    await page.goto("/admin");
    await expect(page.getByRole("heading", { name: "Overview" })).toBeVisible();
    await page.goto("/admin/users");
    await expect(page.getByRole("heading", { name: "Users" })).toBeVisible();
    await page.goto("/admin/audit");
    await expect(page.getByRole("heading", { name: "Audit log" })).toBeVisible();

    await seedBucketEvent(page, origin, bucket, account, note, `playwright-seed-${stamp}`);
    await page.goto(`/dashboard/buckets/${bucket}`);
    await expect(page.getByText("Deposit or charge")).toBeVisible();
    await page.locator("#bucket-account").fill(account);
    await page.locator("#bucket-amount").fill("1");
    await page.locator("#bucket-note").fill(note);
    await page.locator("#bucket-idempotency").fill(`playwright-${stamp}`);
    await page.getByRole("button", { name: "Deposit" }).click();
    await expect(page.getByText("Deposit created")).toBeVisible();

    await page.goto(`/dashboard/buckets/${bucket}?q=${encodeURIComponent(note)}&account=${account}&page=2`);
    await expect(page.getByText("Bucket summary")).toBeVisible();
    await expect(page.getByRole("link", { name: "Write event" })).toHaveAttribute("href", /q=.*account=main.*page=2|account=main.*page=2.*q=/);
    await assertNoFailure(page);

    await page.goto("/admin/users");
    await page.getByLabel("Search users").fill(email);
    await page.getByRole("button", { name: "Search" }).click();
    await expect(page.getByRole("link", { name: email }).first()).toBeVisible();

    await expect.poll(async () => page.url()).toContain(origin);
  } finally {
    await cleanupSmokeKeys(page, origin, keyName);
  }
});

test("prod smoke non-admin account is locked out of admin UI and APIs", async ({ page, request, baseURL }) => {
  const email = process.env.PLAYWRIGHT_SMOKE_NONADMIN_EMAIL;
  test.skip(!email, "PLAYWRIGHT_SMOKE_NONADMIN_EMAIL is required for the non-admin prod smoke");
  const origin = new URL(baseURL || "https://app.shardd.xyz").origin;

  await loginWithResendMagicLink(page, request, email!, baseURL);

  const verify = await page.context().request.get(`${origin}/api/auth/verify`);
  expect(verify.ok(), `auth/verify should succeed: ${verify.status()}`).toBeTruthy();
  const session = await verify.json();
  expect(session.email?.toLowerCase?.()).toBe(email!.toLowerCase());
  expect(session.is_admin, `${email} must not be admin in prod`).toBeFalsy();

  await page.goto("/dashboard");
  await expect(page.getByRole("heading", { name: /Overview|Developer home/ })).toBeVisible();
  const topNav = page.locator(".topbar .nav-row");
  await expect(topNav.getByRole("link", { name: "Developer" })).toHaveCount(0);
  await expect(topNav.getByRole("link", { name: "Admin" })).toHaveCount(0);

  await page.goto("/admin");
  await expect.poll(async () => new URL(page.url()).pathname).toMatch(/^\/dashboard(\/|$)/);
  await expect(page.getByRole("heading", { name: "Developer home" })).toBeVisible();

  const adminUsers = await page.context().request.get(`${origin}/api/admin/users`);
  expect(adminUsers.status(), "non-admin must be forbidden from /api/admin/users").toBe(403);
});

async function seedBucketEvent(page: import("@playwright/test").Page, origin: string, bucket: string, account: string, note: string, idempotencyNonce: string) {
  const response = await page.context().request.post(`${origin}/api/developer/buckets/${encodeURIComponent(bucket)}/events`, {
    data: {
      account,
      amount: 1,
      note,
      idempotency_nonce: idempotencyNonce,
      max_overdraft: null,
      min_acks: null,
      ack_timeout_ms: null,
    },
  });
  expect(response.ok(), `seed event failed: ${response.status()} ${await response.text()}`).toBeTruthy();
}

async function cleanupSmokeKeys(page: import("@playwright/test").Page, origin: string, keyName: string) {
  const response = await page.context().request.get(`${origin}/api/developer/keys`);
  if (!response.ok()) return;
  const keys = await response.json();
  for (const key of Array.isArray(keys) ? keys : []) {
    if (key.name !== keyName || key.revoked_at) continue;
    await page.context().request.post(`${origin}/api/developer/keys/${encodeURIComponent(key.id)}/revoke`);
  }
}

async function dismissFlash(page: import("@playwright/test").Page) {
  const dismiss = page.locator('button[data-action="dismiss-flash"]');
  if (await dismiss.count()) await dismiss.first().click();
}

async function assertNoFailure(page: import("@playwright/test").Page) {
  const body = await page.locator("body").innerText();
  expect(body).not.toContain("Request failed");
  expect(body).not.toContain("q is not defined");
}
