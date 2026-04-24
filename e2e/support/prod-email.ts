import { expect, type Page, type APIRequestContext } from "@playwright/test";
import { randomUUID } from "node:crypto";

type ReceivedEmailRef = {
  id: string;
  to?: string[] | string;
  subject?: string;
  created_at?: string;
};

type ReceivedEmail = ReceivedEmailRef & {
  html?: string | null;
  text?: string | null;
};

const resendBaseUrl = "https://api.resend.com";

export async function loginWithResendMagicLink(page: Page, request: APIRequestContext, email: string, baseURL?: string | null) {
  const apiKey = process.env.RESEND_API_KEY || process.env.SHARDD_DASHBOARD_RESEND_API_KEY || process.env.RESEND_TOKEN;
  expect(apiKey, "RESEND_API_KEY is required for prod smoke login").toBeTruthy();

  const appOrigin = new URL(baseURL || process.env.PLAYWRIGHT_BASE_URL || "https://app.shardd.xyz").origin;
  const loginSession = randomUUID();
  const seenIds = new Set((await listReceivedEmails(request, apiKey!)).map((emailRef) => emailRef.id));
  await page.context().addCookies([{
    name: "login_session",
    value: loginSession,
    domain: new URL(appOrigin).hostname,
    path: "/",
    httpOnly: true,
    sameSite: "Lax",
  }]);

  await page.goto("/login");
  await page.getByLabel("Email").fill(email);
  await page.getByRole("button", { name: /Send magic link/i }).click();
  await page.getByText("Check your inbox").waitFor();
  await expect.poll(async () => {
    const cookies = await page.context().cookies(appOrigin);
    return cookies.some((cookie) => cookie.name === "login_session" && cookie.value === loginSession);
  }, { message: "login_session cookie should be set before consuming the magic link" }).toBeTruthy();

  await consumeNewestMagicLink(page, request, apiKey!, email, seenIds, appOrigin);
}

async function consumeNewestMagicLink(page: Page, request: APIRequestContext, apiKey: string, email: string, seenIds: Set<string>, appOrigin: string) {
  const deadline = Date.now() + Number(process.env.PLAYWRIGHT_RESEND_TIMEOUT_MS || 180_000);
  const triedIds = new Set<string>();
  let lastError = "";
  while (Date.now() < deadline) {
    try {
      const candidate = await findMagicLinkCandidate(request, apiKey, email, seenIds, triedIds, appOrigin);
      if (candidate) {
        triedIds.add(candidate.id);
        await page.goto(candidate.link);
        try {
          await page.getByText(/Authentication successful|Developer home|Overview/).first().waitFor({ timeout: 45_000 });
          return;
        } catch (error) {
          const body = await page.locator("body").innerText().catch(() => "");
          lastError = `candidate ${candidate.id} did not authenticate: ${body.replace(/\s+/g, " ").slice(0, 240)}`;
        }
      }
    } catch (error) {
      lastError = error instanceof Error ? error.message : String(error);
    }
    await new Promise((resolve) => setTimeout(resolve, 3_000));
  }
  throw new Error(`Timed out consuming Resend magic link for ${email}${lastError ? `; last error: ${lastError}` : ""}`);
}

async function findMagicLinkCandidate(request: APIRequestContext, apiKey: string, email: string, seenIds: Set<string>, triedIds: Set<string>, appOrigin: string) {
  const refs = await listReceivedEmails(request, apiKey);
  const candidates = refs
    .filter((ref) => !seenIds.has(ref.id) && !triedIds.has(ref.id) && matchesCandidate(ref, email))
    .sort((a, b) => candidateTime(b) - candidateTime(a));
  for (const ref of candidates) {
    const detail = await retrieveReceivedEmail(request, apiKey, ref.id);
    const link = extractMagicLink(detail, appOrigin);
    if (link) return { id: ref.id, link };
  }
  return null;
}

async function listReceivedEmails(request: APIRequestContext, apiKey: string): Promise<ReceivedEmailRef[]> {
  const response = await request.get(`${resendBaseUrl}/emails/receiving`, {
    headers: { Authorization: `Bearer ${apiKey}` },
  });
  if (!response.ok()) throw new Error(`Resend list received emails failed: ${response.status()} ${await response.text()}`);
  const payload = await response.json();
  return Array.isArray(payload.data) ? payload.data : [];
}

async function retrieveReceivedEmail(request: APIRequestContext, apiKey: string, id: string): Promise<ReceivedEmail> {
  const response = await request.get(`${resendBaseUrl}/emails/receiving/${id}`, {
    headers: { Authorization: `Bearer ${apiKey}` },
  });
  if (!response.ok()) throw new Error(`Resend retrieve received email failed: ${response.status()} ${await response.text()}`);
  return response.json();
}

function matchesCandidate(emailRef: ReceivedEmailRef, email: string) {
  const to = Array.isArray(emailRef.to) ? emailRef.to : emailRef.to ? [emailRef.to] : [];
  const recipients = to.map((value) => value.toLowerCase());
  if (!recipients.includes(email.toLowerCase())) return false;
  if ((emailRef.subject || "").toLowerCase() !== "sign in") return false;
  return true;
}

function extractMagicLink(email: ReceivedEmail, appOrigin: string) {
  const content = `${email.html || ""}\n${email.text || ""}`;
  const match = content.match(new RegExp(`${escapeRegExp(appOrigin)}\\/magic\\?token=[^"'<>\\s]+`));
  return match?.[0].replace(/&amp;/g, "&") || null;
}

function parseResendDate(value?: string) {
  if (!value) return null;
  const normalized = value.includes("T") ? value : value.replace(" ", "T");
  const date = new Date(normalized);
  return Number.isNaN(date.getTime()) ? null : date;
}

function candidateTime(emailRef: ReceivedEmailRef) {
  return parseResendDate(emailRef.created_at)?.getTime() || 0;
}

function escapeRegExp(value: string) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
