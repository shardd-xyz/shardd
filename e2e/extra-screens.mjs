import { chromium } from "playwright";
import { randomUUID } from "node:crypto";

const API_KEY = process.env.RESEND_API_KEY;
const EMAIL = "ops@shardd.xyz";
const BASE = "https://app.shardd.xyz";

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
const page = await ctx.newPage();

const sid = randomUUID();
await ctx.addCookies([{ name: "login_session", value: sid, domain: "app.shardd.xyz", path: "/", httpOnly: true, sameSite: "Lax" }]);

const seen = new Set((await (await fetch("https://api.resend.com/emails/receiving", { headers: { Authorization: `Bearer ${API_KEY}` } })).json()).data.map(e => e.id));

await fetch(`${BASE}/api/auth/request`, {
  method: "POST",
  headers: { "Content-Type": "application/json", Cookie: `login_session=${sid}` },
  body: JSON.stringify({ email: EMAIL }),
});

const deadline = Date.now() + 120_000;
let link = null;
while (Date.now() < deadline && !link) {
  const emails = (await (await fetch("https://api.resend.com/emails/receiving", { headers: { Authorization: `Bearer ${API_KEY}` } })).json()).data || [];
  for (const ref of emails.filter(r => !seen.has(r.id))) {
    const to = Array.isArray(ref.to) ? ref.to : [ref.to];
    if (!to.map(v => (v || "").toLowerCase()).includes(EMAIL)) continue;
    if ((ref.subject || "").toLowerCase() !== "sign in") continue;
    const detail = await (await fetch(`https://api.resend.com/emails/receiving/${ref.id}`, { headers: { Authorization: `Bearer ${API_KEY}` } })).json();
    const content = `${detail.html || ""}\n${detail.text || ""}`;
    const esc = BASE.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const m = content.match(new RegExp(`${esc}/magic\\?token=[^"'<>\\s]+`));
    if (m) { link = m[0].replace(/&amp;/g, "&"); break; }
  }
  if (!link) await new Promise(r => setTimeout(r, 3000));
}

const token = new URL(link).searchParams.get("token");
await page.context().request.post(`${BASE}/api/auth/consume`, { data: { token } });

const routes = [
  ["/dashboard/billing", "extra-billing"],
  ["/dashboard/contact?plan=enterprise", "extra-contact"],
];
for (const [path, name] of routes) {
  await page.goto(`${BASE}${path}`);
  await page.waitForTimeout(3500);
  await page.screenshot({ path: `/tmp/${name}.png`, fullPage: true });
  console.log(`${name}: done`);
}

// Navigate to /dashboard/buckets and click the first bucket to capture the quickstart card.
await page.goto(`${BASE}/dashboard/buckets`);
await page.waitForTimeout(3000);
const href = await page.locator('a[href*="/dashboard/buckets/"]').first().getAttribute("href").catch(() => null);
if (href) {
  await page.goto(`${BASE}${href}`);
  await page.waitForTimeout(3500);
  await page.screenshot({ path: "/tmp/extra-bucket-detail.png", fullPage: true });
  console.log("extra-bucket-detail: done");
} else {
  console.log("no buckets found");
}

// ⌘K palette: open it with keyboard shortcut.
await page.goto(`${BASE}/dashboard`);
await page.waitForTimeout(2500);
await page.keyboard.press("Meta+k");
await page.waitForTimeout(500);
await page.screenshot({ path: "/tmp/extra-palette.png", fullPage: false });
console.log("extra-palette: done");

await browser.close();
console.log("ALL DONE");
