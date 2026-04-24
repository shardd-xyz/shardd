import { chromium } from "playwright";
import { randomUUID } from "node:crypto";
import { mkdir } from "node:fs/promises";

const BASE_URL = process.env.PLAYWRIGHT_BASE_URL || "https://app.shardd.xyz";
const EMAIL = process.env.PLAYWRIGHT_SMOKE_EMAIL || "ops@shardd.xyz";
const OUT = process.env.SCREENSHOT_DIR || "/tmp/shardd-prod-screenshots";
const API_KEY =
  process.env.RESEND_API_KEY ||
  process.env.SHARDD_DASHBOARD_RESEND_API_KEY ||
  process.env.RESEND_TOKEN;
if (!API_KEY) throw new Error("RESEND_API_KEY required");

await mkdir(OUT, { recursive: true });
const origin = new URL(BASE_URL).origin;

async function listEmails() {
  const res = await fetch("https://api.resend.com/emails/receiving", {
    headers: { Authorization: `Bearer ${API_KEY}` },
  });
  if (!res.ok) throw new Error(`list failed: ${res.status}`);
  const body = await res.json();
  return Array.isArray(body.data) ? body.data : [];
}

async function retrieveEmail(id) {
  const res = await fetch(`https://api.resend.com/emails/receiving/${id}`, {
    headers: { Authorization: `Bearer ${API_KEY}` },
  });
  if (!res.ok) throw new Error(`retrieve failed: ${res.status}`);
  return res.json();
}

function extractLink(email) {
  const content = `${email.html || ""}\n${email.text || ""}`;
  const esc = origin.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const m = content.match(new RegExp(`${esc}\\/magic\\?token=[^"'<>\\s]+`));
  return m?.[0].replace(/&amp;/g, "&") || null;
}

async function login(page) {
  const sid = randomUUID();
  const seen = new Set((await listEmails()).map((e) => e.id));
  await page.context().addCookies([
    {
      name: "login_session",
      value: sid,
      domain: new URL(origin).hostname,
      path: "/",
      httpOnly: true,
      sameSite: "Lax",
    },
  ]);
  await page.goto(`${origin}/login`);
  await page.getByLabel("Email").fill(EMAIL);
  await page.getByRole("button", { name: /Send magic link/i }).click();
  await page.getByText("Check your inbox").waitFor();

  const deadline = Date.now() + 180_000;
  const tried = new Set();
  while (Date.now() < deadline) {
    const refs = (await listEmails())
      .filter((r) => !seen.has(r.id) && !tried.has(r.id))
      .filter((r) => {
        const to = Array.isArray(r.to) ? r.to : [r.to];
        if (!to.map((v) => (v || "").toLowerCase()).includes(EMAIL.toLowerCase())) return false;
        return (r.subject || "").toLowerCase() === "sign in";
      })
      .sort((a, b) => new Date(b.created_at) - new Date(a.created_at));
    for (const r of refs) {
      tried.add(r.id);
      const detail = await retrieveEmail(r.id);
      const link = extractLink(detail);
      if (link) {
        await page.goto(link);
        await page.waitForURL((url) => !url.toString().includes("/magic"), { timeout: 45_000 });
        return;
      }
    }
    await new Promise((r) => setTimeout(r, 3000));
  }
  throw new Error(`Timed out consuming magic link for ${EMAIL}`);
}

async function visit(page, path) {
  await page.goto(`${BASE_URL}${path}`);
  await page.waitForLoadState("networkidle", { timeout: 30_000 }).catch(() => {});
  await page.waitForSelector(".page-title, .auth-shell", { timeout: 15_000 }).catch(() => {});
}

async function shot(page, name) {
  const path = `${OUT}/${name}.png`;
  await page.screenshot({ path, fullPage: true });
  console.log(path);
}

const browser = await chromium.launch();
try {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await ctx.newPage();
  await login(page);

  await visit(page, "/dashboard");
  await shot(page, "01-desktop-dashboard-home");

  await visit(page, "/dashboard/keys");
  await shot(page, "02-desktop-dashboard-keys");

  await visit(page, "/dashboard/buckets");
  await shot(page, "03-desktop-dashboard-buckets");

  const firstHref = await page
    .locator("a[href^='/dashboard/buckets/']")
    .first()
    .getAttribute("href")
    .catch(() => null);

  if (firstHref && /\/dashboard\/buckets\/[^/]+/.test(firstHref)) {
    await visit(page, firstHref);
    await shot(page, "04-desktop-bucket-detail");
    await visit(page, firstHref + "?account=main");
    await shot(page, "05-desktop-bucket-detail-filtered");
    await visit(page, firstHref + "?tab=accounts");
    const firstAccountHref = await page
      .locator(`a[href^='${firstHref}/accounts/']`)
      .first()
      .getAttribute("href")
      .catch(() => null);
    if (firstAccountHref) {
      await visit(page, firstAccountHref);
      await shot(page, "06-desktop-account-detail");
    }
  } else {
    console.log("no bucket detail link found");
  }

  await visit(page, "/profile");
  await shot(page, "06-desktop-profile");

  await visit(page, "/admin");
  await shot(page, "07-desktop-admin-overview");

  await visit(page, "/admin/users");
  await shot(page, "08-desktop-admin-users");

  await visit(page, "/admin/audit");
  await shot(page, "09-desktop-admin-audit");

  await ctx.close();

  const mctx = await browser.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 2 });
  const mpage = await mctx.newPage();
  await login(mpage);

  await visit(mpage, "/dashboard");
  await shot(mpage, "20-mobile-dashboard-home");

  if (firstHref) {
    await visit(mpage, firstHref);
    await shot(mpage, "21-mobile-bucket-detail");
  }

  await visit(mpage, "/dashboard/keys");
  await shot(mpage, "22-mobile-dashboard-keys");
} finally {
  await browser.close();
}
