import { describe, it, expect, vi } from "vitest";
import { Client, InsufficientFundsError } from "../src/index.js";

// These tests stub `fetch` to exercise the failover + error-mapping
// code without hitting a live gateway.

function mockFetch(
  responses: Array<{ url: RegExp; status: number; body: unknown }>,
) {
  return vi.fn(async (input: RequestInfo | URL) => {
    const url = typeof input === "string" ? input : input.toString();
    for (const r of responses) {
      if (r.url.test(url)) {
        return new Response(JSON.stringify(r.body), {
          status: r.status,
          headers: { "Content-Type": "application/json" },
        });
      }
    }
    throw new Error(`unexpected URL in mock: ${url}`);
  });
}

describe("Client", () => {
  it("picks the fastest healthy edge and posts an event", async () => {
    const healthOk = {
      edge_id: "use1",
      region: "us-east-1",
      base_url: "https://use1.api.shardd.xyz",
      ready: true,
      discovered_nodes: 3,
      healthy_nodes: 3,
      best_node_rtt_ms: 5,
      sync_gap: 0,
      overloaded: false,
      auth_enabled: true,
    };
    const fetchImpl = mockFetch([
      { url: /\/gateway\/health/, status: 200, body: healthOk },
      {
        url: /\/events$/,
        status: 201,
        body: {
          event: {
            event_id: "evt-1",
            origin_node_id: "n1",
            origin_epoch: 1,
            origin_seq: 42,
            created_at_unix_ms: Date.now(),
            type: "standard",
            bucket: "demo",
            account: "alice",
            amount: 500,
            note: "test",
            idempotency_nonce: "nonce-1",
            void_ref: null,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
          },
          balance: 500,
          available_balance: 500,
          deduplicated: false,
          acks: { requested: 1, received: 1, timeout: false },
        },
      },
    ]);

    const client = new Client("test-key", {
      fetch: fetchImpl as unknown as typeof fetch,
    });
    const result = await client.createEvent("demo", "alice", 500);
    expect(result.event.event_id).toBe("evt-1");
    expect(result.balance).toBe(500);
    expect(result.deduplicated).toBe(false);
  });

  it("listMyBuckets passes query and decodes the response", async () => {
    const healthOk = {
      edge_id: "use1",
      region: "us-east-1",
      base_url: "https://use1.api.shardd.xyz",
      ready: true,
      discovered_nodes: 3,
      healthy_nodes: 3,
      best_node_rtt_ms: 5,
      sync_gap: 0,
      overloaded: false,
      auth_enabled: true,
    };
    const seenUrls: string[] = [];
    const seenHeaders: Headers[] = [];
    const fetchImpl = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input.toString();
      seenUrls.push(url);
      if (init?.headers) seenHeaders.push(new Headers(init.headers));
      if (/\/gateway\/health/.test(url)) {
        return new Response(JSON.stringify(healthOk), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        });
      }
      if (/\/v1\/me\/buckets($|\?)/.test(url)) {
        return new Response(
          JSON.stringify({
            buckets: [
              {
                bucket: "demo",
                total_balance: 1000,
                available_balance: 900,
                active_hold_total: 100,
                account_count: 2,
                event_count: 5,
                last_event_at_unix_ms: 1700000000000,
              },
            ],
            total: 1,
            page: 1,
            limit: 25,
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      throw new Error(`unexpected URL in mock: ${url}`);
    });

    const client = new Client("dash-token", {
      fetch: fetchImpl as unknown as typeof fetch,
    });
    const result = await client.listMyBuckets({ page: 1, limit: 25, q: "demo" });

    expect(result.total).toBe(1);
    expect(result.buckets[0]!.bucket).toBe("demo");
    const apiUrl = seenUrls.find((u) => u.includes("/v1/me/buckets"))!;
    expect(apiUrl).toMatch(/page=1/);
    expect(apiUrl).toMatch(/limit=25/);
    expect(apiUrl).toMatch(/q=demo/);
    expect(seenHeaders.at(-1)!.get("Authorization")).toBe("Bearer dash-token");
  });

  it("withApiKey reuses selector and swaps the bearer token", async () => {
    const healthOk = {
      edge_id: "use1",
      region: "us-east-1",
      base_url: "https://use1.api.shardd.xyz",
      ready: true,
      discovered_nodes: 3,
      healthy_nodes: 3,
      best_node_rtt_ms: 5,
      sync_gap: 0,
      overloaded: false,
      auth_enabled: true,
    };
    let healthHits = 0;
    const seenAuth: Array<string | null> = [];
    const fetchImpl = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input.toString();
      if (/\/gateway\/health/.test(url)) {
        healthHits++;
        return new Response(JSON.stringify(healthOk), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        });
      }
      if (/\/v1\/me\/buckets\/deleted/.test(url)) {
        seenAuth.push(new Headers(init?.headers).get("Authorization"));
        return new Response(JSON.stringify({ buckets: [] }), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        });
      }
      throw new Error(`unexpected URL in mock: ${url}`);
    });

    const client = new Client("first-token", {
      fetch: fetchImpl as unknown as typeof fetch,
    });
    await client.listMyDeletedBuckets();
    const swapped = client.withApiKey("second-token");
    await swapped.listMyDeletedBuckets();

    // Selector is shared, so probing should only happen once.
    expect(healthHits).toBe(3);
    expect(seenAuth).toEqual(["Bearer first-token", "Bearer second-token"]);
  });

  it("surfaces 422 as InsufficientFundsError with balance fields", async () => {
    const healthOk = {
      edge_id: "use1",
      region: "us-east-1",
      base_url: "https://use1.api.shardd.xyz",
      ready: true,
      discovered_nodes: 3,
      healthy_nodes: 3,
      best_node_rtt_ms: 5,
      sync_gap: 0,
      overloaded: false,
      auth_enabled: true,
    };
    const fetchImpl = mockFetch([
      { url: /\/gateway\/health/, status: 200, body: healthOk },
      {
        url: /\/events$/,
        status: 422,
        body: {
          error: "insufficient funds",
          balance: 10,
          available_balance: 10,
          limit: 0,
        },
      },
    ]);
    const client = new Client("test-key", {
      fetch: fetchImpl as unknown as typeof fetch,
    });
    await expect(
      client.createEvent("demo", "alice", -100),
    ).rejects.toBeInstanceOf(InsufficientFundsError);
  });
});
