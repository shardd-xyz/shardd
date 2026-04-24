// Failover integration test.
//
// Runs only when `SHARDD_FAILOVER_GATEWAYS` is set — a comma-separated
// list of local gateway URLs. `./run sdk:test:failover` brings up the
// 3-gateway harness, sets the env, and invokes vitest.
import { describe, expect, it } from "vitest";
import { Client } from "../src/index.js";

const raw = process.env.SHARDD_FAILOVER_GATEWAYS;
const bucket = process.env.SHARDD_FAILOVER_BUCKET ?? "failover-test";
const GATEWAYS = raw
  ? raw.split(",").map((s) => s.trim()).filter(Boolean)
  : [];
const ENABLED = GATEWAYS.length > 0;

// vitest's `describe.skip` keeps the suite visible in the report but
// skips each test when the env var is absent — keeps CI output honest.
const maybe = ENABLED ? describe : describe.skip;

maybe("failover against docker harness", () => {
  it("all-healthy: probe picks one, write + idempotent replay succeed", async () => {
    const client = new Client("local-dev", { edges: GATEWAYS });
    const first = await client.createEvent(bucket, "alice", 10, {
      note: "failover test: phase A",
    });
    expect(first.deduplicated).toBe(false);

    const replay = await client.createEvent(bucket, "alice", 10, {
      idempotencyNonce: first.event.idempotency_nonce,
    });
    expect(replay.deduplicated).toBe(true);
    expect(replay.event.event_id).toBe(first.event.event_id);
  });

  it("closed port mixed in: probe skips it", async () => {
    const edges = ["http://127.0.0.1:1", ...GATEWAYS];
    const client = new Client("local-dev", { edges });
    const result = await client.createEvent(bucket, "bob", 5, {
      note: "failover test: phase B",
    });
    expect(result.deduplicated).toBe(false);
  });

  it("single survivor: writes route to the one healthy edge", async () => {
    const [survivor] = GATEWAYS;
    const client = new Client("local-dev", {
      edges: ["http://127.0.0.1:1", "http://127.0.0.1:2", survivor!],
    });
    const result = await client.createEvent(bucket, "carol", 7, {
      note: "failover test: phase C",
    });
    expect(result.deduplicated).toBe(false);
  });

  it(
    "mid-run outage (gated on SHARDD_FAILOVER_KILLED_GATEWAY): writes still succeed",
    async () => {
      if (!process.env.SHARDD_FAILOVER_KILLED_GATEWAY) {
        return; // not in kill-phase of the harness
      }
      const client = new Client("local-dev", { edges: GATEWAYS });
      const result = await client.createEvent(bucket, "dan", 3, {
        note: "failover test: phase D — mid-outage",
      });
      expect(result.deduplicated).toBe(false);
    },
  );
});
