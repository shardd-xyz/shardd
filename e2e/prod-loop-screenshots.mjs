// FULL LOOP prod-validation screenshots. See CLAUDE.md → FULL LOOP.
//
// Hits the public landing surfaces and the app login page on prod,
// captures full-page PNGs into /tmp/prod-screenshots/, and prints one
// "OK <name> <bytes>" line per shot so the agent can spot rendering
// regressions in the surface it just changed.
//
// No login flow here — for authenticated screenshots use
// e2e/dx-screenshot.mjs / extra-screens.mjs which drive the magic-link
// path with RESEND_API_KEY.

import { chromium } from "playwright";
import { mkdir, stat } from "node:fs/promises";

const OUT = process.env.SCREENSHOT_DIR || "/tmp/prod-screenshots";
await mkdir(OUT, { recursive: true });

const TARGETS = [
  ["landing-home", "https://shardd.xyz/"],
  ["landing-quickstart", "https://shardd.xyz/guide/quickstart"],
  ["landing-sdks", "https://shardd.xyz/guide/sdks"],
  ["landing-public-edge-clients", "https://shardd.xyz/guide/public-edge-clients"],
  ["app-login", "https://app.shardd.xyz/"],
];

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
const page = await ctx.newPage();

let failed = 0;
for (const [name, url] of TARGETS) {
  const out = `${OUT}/${name}.png`;
  try {
    const resp = await page.goto(url, { waitUntil: "networkidle", timeout: 30_000 });
    const status = resp ? resp.status() : 0;
    await page.screenshot({ path: out, fullPage: true });
    const size = (await stat(out)).size;
    const tag = status >= 200 && status < 400 ? "OK" : "BAD";
    console.log(`${tag} ${name} status=${status} bytes=${size} url=${url}`);
    if (tag !== "OK") failed++;
  } catch (err) {
    console.log(`ERR ${name} url=${url} ${err.message}`);
    failed++;
  }
}

await browser.close();
process.exit(failed === 0 ? 0 : 1);
