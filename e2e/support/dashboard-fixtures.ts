import { expect, type Page, type Route } from "@playwright/test";

type Session = { id: string; email: string; is_admin: boolean };
type KeyRecord = {
  id: string;
  name: string;
  key_prefix: string;
  created_at: string;
  updated_at: string;
  last_used_at: string | null;
  revoked_at: string | null;
  expires_at: string | null;
  user_id: string;
};
type ScopeRecord = {
  id: string;
  api_key_id: string;
  resource_type: string;
  match_type: string;
  resource_value: string | null;
  can_read: boolean;
  can_write: boolean;
  created_at: string;
};

const nowIso = "2026-04-16T09:00:00Z";
const developer: Session = {
  id: "11111111-1111-4111-8111-111111111111",
  email: "developer@example.test",
  is_admin: false,
};
const admin: Session = {
  id: "22222222-2222-4222-8222-222222222222",
  email: "admin@example.test",
  is_admin: true,
};
const targetUserId = "33333333-3333-4333-8333-333333333333";

export const smokeBucket = "orders";
export const smokeAccount = "main";
export const smokeNote = "codex smoke event";

export async function installMockApi(page: Page, options: { session?: "developer" | "admin" | null } = {}) {
  const currentSession = { value: options.session === "admin" ? admin : options.session === null ? null : developer };
  const targetUser = makeTargetUser();
  const keys: KeyRecord[] = [];
  const scopes = new Map<string, ScopeRecord[]>();
  let keyCounter = 0;
  let scopeCounter = 0;
  let impersonating = false;

  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;
    const method = request.method();

    if (path === "/api/auth/verify" && method === "GET") {
      if (!currentSession.value) return json(route, { code: "INVALID_CREDENTIALS" }, 401);
      return json(route, currentSession.value);
    }
    if (path === "/api/auth/logout" && method === "POST") {
      currentSession.value = null;
      return json(route, {});
    }
    if (path === "/api/auth/request" && method === "POST") {
      return json(route, {}, 202, { "set-cookie": "login_session=mock-session; Path=/; HttpOnly; SameSite=Lax" });
    }
    if (path === "/api/auth/consume" && method === "POST") {
      currentSession.value = developer;
      return json(route, {});
    }
    if (!currentSession.value) return json(route, { code: "INVALID_CREDENTIALS" }, 401);

    if (path === "/api/developer/me" && method === "GET") {
      return json(route, { id: currentSession.value.id, is_frozen: false });
    }
    if (path === "/api/developer/keys" && method === "GET") return json(route, keys);
    if (path === "/api/developer/keys" && method === "POST") {
      const body = await request.postDataJSON();
      const key = makeKey(++keyCounter, body.name, currentSession.value.id);
      keys.unshift(key);
      scopes.set(key.id, []);
      return json(route, { api_key: key, raw_key: `shardd_test_${key.id}` }, 201);
    }

    const keyScopeMatch = path.match(/^\/api\/developer\/keys\/([^/]+)\/scopes$/);
    if (keyScopeMatch && method === "GET") return json(route, scopes.get(keyScopeMatch[1]) || []);
    if (keyScopeMatch && method === "POST") {
      const body = await request.postDataJSON();
      const scope = makeScope(++scopeCounter, keyScopeMatch[1], body);
      scopes.get(keyScopeMatch[1])?.push(scope);
      return json(route, scope, 201);
    }

    const keyActionMatch = path.match(/^\/api\/developer\/keys\/([^/]+)\/(rotate|revoke)$/);
    if (keyActionMatch && method === "POST") {
      const key = keys.find((entry) => entry.id === keyActionMatch[1]);
      if (!key) return json(route, { code: "NOT_FOUND" }, 404);
      if (keyActionMatch[2] === "revoke") {
        key.revoked_at = nowIso;
        return json(route, {}, 204);
      }
      key.key_prefix = `shd_rot_${keyCounter}`;
      return json(route, { api_key: key, raw_key: `shardd_rotated_${key.id}` }, 201);
    }

    const scopeDeleteMatch = path.match(/^\/api\/developer\/scopes\/([^/]+)$/);
    if (scopeDeleteMatch && method === "DELETE") {
      for (const [keyId, keyScopes] of scopes) {
        scopes.set(keyId, keyScopes.filter((scope) => scope.id !== scopeDeleteMatch[1]));
      }
      return json(route, {}, 204);
    }

    if (path === "/api/developer/buckets" && method === "GET") {
      return json(route, { total: 1, buckets: [bucketSummary()] });
    }
    if (path === `/api/developer/buckets/${smokeBucket}` && method === "GET") return json(route, bucketDetail());
    if (path === `/api/developer/buckets/${smokeBucket}/events` && method === "GET") {
      return json(route, { total: 30, events: [bucketEvent()] });
    }
    if (path === `/api/developer/buckets/${smokeBucket}/events` && method === "POST") {
      return json(route, { event_id: "evt_created", deduplicated: false }, 201);
    }

    if (path === "/api/admin/stats" && method === "GET") {
      return json(route, { total_users: 3, users_last_7_days: 1, users_last_30_days: 2, frozen_users: 0, admin_users: 1 });
    }
    if (path === "/api/admin/users" && method === "GET") return json(route, { users: [targetUser], total: 1, limit: 50, offset: 0 });
    if (path === `/api/admin/users/${targetUser.id}` && method === "GET") return json(route, targetUser);
    if (path === `/api/admin/users/${targetUser.id}/developer/keys` && method === "GET") return json(route, keys);
    if (path === `/api/admin/users/${targetUser.id}/freeze` && method === "POST") {
      targetUser.is_frozen = true;
      return json(route, {});
    }
    if (path === `/api/admin/users/${targetUser.id}/unfreeze` && method === "POST") {
      targetUser.is_frozen = false;
      return json(route, {});
    }
    if (path === `/api/admin/users/${targetUser.id}/impersonate` && method === "POST") {
      currentSession.value = { id: targetUser.id, email: targetUser.email, is_admin: false };
      impersonating = true;
      return json(route, {}, 200, { "set-cookie": `impersonating=${encodeURIComponent(targetUser.email)}; Path=/; SameSite=Lax` });
    }
    if (path === "/api/admin/audit" && method === "GET") {
      return json(route, { total: 1, limit: 100, offset: 0, entries: [{ id: "audit-1", admin_id: admin.id, admin_email: admin.email, action: "user.impersonate", target_user_id: targetUser.id, target_email: targetUser.email, metadata: {}, created_at: nowIso }] });
    }

    return json(route, { code: "NOT_FOUND", path, method, impersonating }, 404);
  });
}

export async function assertHealthy(page: Page) {
  const body = await page.locator("body").innerText();
  expect(body).not.toContain("Request failed");
  expect(body).not.toContain("q is not defined");
}

async function json(route: Route, payload: unknown, status = 200, headers: Record<string, string> = {}) {
  await route.fulfill({
    status,
    headers: { "content-type": "application/json", ...headers },
    body: status === 204 ? "" : JSON.stringify(payload),
  });
}

function makeKey(index: number, name: string, userId: string): KeyRecord {
  return {
    id: `key-${index}`,
    name,
    key_prefix: `shd_${index}`,
    created_at: nowIso,
    updated_at: nowIso,
    last_used_at: null,
    revoked_at: null,
    expires_at: null,
    user_id: userId,
  };
}

function makeScope(index: number, apiKeyId: string, body: { match_type: string; bucket?: string | null; can_read: boolean; can_write: boolean }): ScopeRecord {
  return {
    id: `scope-${index}`,
    api_key_id: apiKeyId,
    resource_type: "bucket",
    match_type: body.match_type,
    resource_value: body.bucket || null,
    can_read: body.can_read,
    can_write: body.can_write,
    created_at: nowIso,
  };
}

function makeTargetUser() {
  return {
    id: targetUserId,
    email: "target@example.test",
    language: "en",
    created_at: nowIso,
    updated_at: nowIso,
    last_login_at: nowIso,
    is_admin: false,
    is_frozen: false,
  };
}

function bucketSummary() {
  return {
    bucket: smokeBucket,
    account_count: 1,
    event_count: 30,
    total_balance: 124,
    available_balance: 124,
    active_hold_total: 0,
    last_event_at_unix_ms: 1_710_000_000_000,
  };
}

function bucketDetail() {
  return {
    summary: bucketSummary(),
    accounts: [{
      account: smokeAccount,
      balance: 124,
      available_balance: 124,
      active_hold_total: 0,
      event_count: 30,
      last_event_at_unix_ms: 1_710_000_000_000,
    }],
  };
}

function bucketEvent() {
  return {
    created_at_unix_ms: 1_710_000_000_000,
    event_id: "evt_smoke",
    idempotency_nonce: "nonce_smoke",
    account: smokeAccount,
    type: "standard",
    amount: 123,
    note: smokeNote,
  };
}
