import { createHmac } from "node:crypto";

const SECRET = process.env.SAASSY_JWT_SECRET;
if (!SECRET) { console.error("set SAASSY_JWT_SECRET"); process.exit(1); }

const USER_ID = "d1de115f-70d5-4388-b95e-2eb5ab5fde96"; // emil
const BASE = "https://app.shardd.xyz";

function b64url(buf) {
  return Buffer.from(buf).toString("base64").replace(/=/g, "").replace(/\+/g, "-").replace(/\//g, "_");
}

function jwt(sub) {
  const header = { alg: "HS256", typ: "JWT" };
  const now = Math.floor(Date.now() / 1000);
  const claims = { sub, iat: now, exp: now + 3600 };
  const h = b64url(JSON.stringify(header));
  const c = b64url(JSON.stringify(claims));
  const sig = b64url(createHmac("sha256", SECRET).update(`${h}.${c}`).digest());
  return `${h}.${c}.${sig}`;
}

const token = jwt(USER_ID);
const headers = { "Cookie": `access_token=${token}`, "Content-Type": "application/json" };

const bucketRes = await fetch(`${BASE}/api/developer/buckets`, {
  method: "POST", headers, body: JSON.stringify({ name: "imagechat" }),
});
console.log("create bucket:", bucketRes.status, await bucketRes.text());

const keyRes = await fetch(`${BASE}/api/developer/keys`, {
  method: "POST", headers, body: JSON.stringify({ name: "imagechat" }),
});
const keyBody = await keyRes.json();
console.log("create key:", keyRes.status);
console.log("api_key id:", keyBody.api_key?.id);
console.log("RAW KEY:", keyBody.raw_key);

const scopeRes = await fetch(`${BASE}/api/developer/keys/${keyBody.api_key.id}/scopes`, {
  method: "POST", headers,
  body: JSON.stringify({ match_type: "exact", bucket: "imagechat", can_read: true, can_write: true }),
});
console.log("add bucket scope:", scopeRes.status, await scopeRes.text());

const ctrlRes = await fetch(`${BASE}/api/developer/keys/${keyBody.api_key.id}/scopes`, {
  method: "POST", headers,
  body: JSON.stringify({ match_type: "all", can_read: true, can_write: true }),
});
console.log("add control scope:", ctrlRes.status, await ctrlRes.text());
