import { expect, test } from "@playwright/test";
import { assertHealthy, installMockApi, smokeAccount, smokeBucket, smokeNote } from "../support/dashboard-fixtures";

test("unauthenticated bucket detail preserves next path through login", async ({ page }) => {
  await installMockApi(page, { session: null });
  const nextPath = `/dashboard/buckets/${smokeBucket}?q=${encodeURIComponent(smokeNote)}&account=${smokeAccount}&page=2`;

  await page.goto(nextPath);

  await expect(page).toHaveURL(/\/login\?/);
  expect(new URL(page.url()).searchParams.get("next")).toBe(nextPath);
  await expect(page.getByRole("heading", { name: "Sign in" })).toBeVisible();
  await expect.poll(() => page.evaluate(() => window.localStorage.getItem("shardd-post-login-next"))).toBe(nextPath);
});

test("bucket detail restores query state and renders event/account views", async ({ page }) => {
  await installMockApi(page);

  await page.goto(`/dashboard/buckets/${smokeBucket}?q=${encodeURIComponent(smokeNote)}&account=${smokeAccount}&page=2`);

  await expect(page.getByText("Bucket summary")).toBeVisible();
  await expect(page.locator(".topbar .nav-row").getByRole("link", { name: "Developer" })).toHaveCount(0);
  await expect(page.getByText(smokeNote)).toBeVisible();
  await expect(page.getByRole("link", { name: "Write event" })).toHaveAttribute("href", /q=.*account=main.*page=2|account=main.*page=2.*q=/);
  await page.getByRole("link", { name: "Accounts" }).click();
  await expect(page.getByRole("heading", { name: "Accounts" })).toBeVisible();
  await expect(page.getByText(smokeAccount)).toBeVisible();
  await assertHealthy(page);
});

test("developer key flow handles flash modal before scope and revoke actions", async ({ page }) => {
  await installMockApi(page);

  await page.goto("/dashboard/keys");
  await page.getByLabel("Key name").fill("smoke key");
  await page.getByRole("button", { name: "Create key" }).click();
  await expect(page.getByText("API key created")).toBeVisible();
  await page.locator('button[data-action="dismiss-flash"]').click();

  const keyCard = page.locator(".key-card").filter({ hasText: "smoke key" }).first();
  await keyCard.locator("summary").filter({ hasText: "Scopes" }).click();
  await expect(keyCard.locator('select[name="match_type"]')).toBeVisible();
  await keyCard.locator('select[name="match_type"]').selectOption("exact");
  await keyCard.locator('input[name="bucket"]').fill(smokeBucket);
  await keyCard.locator('input[name="can_write"]').check();
  await keyCard.getByRole("button", { name: "Add scope" }).click();
  await expect(page.getByText("Scope added")).toBeVisible();

  await keyCard.locator('button[data-action="rotate-key"]:not([disabled])').click();
  await expect(page.getByText("API key rotated")).toBeVisible();
  await page.locator('button[data-action="dismiss-flash"]').click();
  page.once("dialog", (dialog) => dialog.accept());
  await keyCard.locator('button[data-action="revoke-key"]:not([disabled])').click();
  await expect(page.getByText("API key revoked")).toBeVisible();
  await assertHealthy(page);
});

test("admin impersonation refetches the replaced session", async ({ page }) => {
  await installMockApi(page, { session: "admin" });

  await page.goto("/admin/users/33333333-3333-4333-8333-333333333333");
  await expect(page.locator(".topbar .nav-row").getByRole("link", { name: "Developer" })).toBeVisible();
  await expect(page.locator(".topbar .nav-row").getByRole("link", { name: "Admin" })).toBeVisible();
  await expect(page.getByText("Account actions")).toBeVisible();
  page.once("dialog", (dialog) => dialog.accept());
  await page.locator('button[data-action="impersonate-user"]:not([disabled])').last().click();

  await expect(page.getByRole("heading", { name: "Developer home" })).toBeVisible();
  await expect(page.getByText("Impersonating target@example.test")).toBeVisible();
  await assertHealthy(page);
});
