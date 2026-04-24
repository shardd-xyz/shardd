// Run with:
//   SHARDD_API_KEY=sk_live_... npm run example:quickstart
// or directly:
//   SHARDD_API_KEY=sk_live_... tsx examples/quickstart.ts
//
// Creates a credit event, reads it back, and replays with the same
// nonce to demonstrate idempotency.

import { Client } from "../src/index.js";

async function main() {
  const apiKey = process.env.SHARDD_API_KEY;
  if (!apiKey) {
    console.error("set SHARDD_API_KEY in your environment");
    process.exit(1);
  }
  const bucket = process.env.SHARDD_BUCKET ?? "demo";
  const shardd = new Client(apiKey);

  // 1. Credit 500 units to user:alice.
  const first = await shardd.createEvent(bucket, "user:alice", 500, {
    note: "sdk quickstart credit",
  });
  console.log(
    `credited: event=${first.event.event_id} balance=${first.balance} deduplicated=${first.deduplicated}`,
  );

  // 2. Retry the same operation — returns the original event.
  const replay = await shardd.createEvent(bucket, "user:alice", 500, {
    note: "sdk quickstart credit",
    idempotencyNonce: first.event.idempotency_nonce,
  });
  console.log(
    `retried:  event=${replay.event.event_id} balance=${replay.balance} deduplicated=${replay.deduplicated}`,
  );

  // 3. Read back the bucket.
  const balances = await shardd.getBalances(bucket);
  for (const row of balances.accounts) {
    console.log(`  ${row.account} = ${row.balance} (available ${row.available_balance})`);
  }

  // 4. Inspect edge selection.
  const h = await shardd.health();
  console.log(`pinned edge: ${h.edge_id} (region ${h.region}, sync_gap ${h.sync_gap})`);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
