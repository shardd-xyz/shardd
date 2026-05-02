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
console.log("magic link requested");

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

if (!link) { console.log("LOGIN FAILED"); await browser.close(); process.exit(1); }

const token = new URL(link).searchParams.get("token");
const consumeRes = await page.context().request.post(`${BASE}/api/auth/consume`, {
  data: { token },
});
console.log("consume:", consumeRes.status());

await page.goto(`${BASE}/dashboard/billing`);
await page.waitForTimeout(5000);
console.log("billing url:", page.url());
await page.screenshot({ path: "/tmp/dx-billing.png", fullPage: true });
console.log("billing screenshot done");

await browser.close();
