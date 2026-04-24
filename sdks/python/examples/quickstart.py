"""Run with:
    SHARDD_API_KEY=sk_live_... python examples/quickstart.py

Creates a credit event, reads it back, and replays with the same nonce
to demonstrate idempotency.
"""
import os
import sys

from shardd import Shardd


def main() -> None:
    api_key = os.environ.get("SHARDD_API_KEY")
    if not api_key:
        print("set SHARDD_API_KEY in your environment", file=sys.stderr)
        sys.exit(1)
    bucket = os.environ.get("SHARDD_BUCKET", "demo")

    with Shardd(api_key) as shardd:
        # 1. Credit 500 units to user:alice.
        first = shardd.create_event(
            bucket, "user:alice", 500, note="sdk quickstart credit"
        )
        print(
            f"credited: event={first.event.event_id} "
            f"balance={first.balance} deduplicated={first.deduplicated}"
        )

        # 2. Retry with the same nonce — should be a no-op.
        replay = shardd.create_event(
            bucket,
            "user:alice",
            500,
            note="sdk quickstart credit",
            idempotency_nonce=first.event.idempotency_nonce,
        )
        print(
            f"retried:  event={replay.event.event_id} "
            f"balance={replay.balance} deduplicated={replay.deduplicated}"
        )
        assert first.event.event_id == replay.event.event_id

        # 3. Read back the bucket.
        balances = shardd.get_balances(bucket)
        for row in balances.accounts:
            print(
                f"  {row.account} = {row.balance} "
                f"(available {row.available_balance})"
            )

        # 4. Inspect edge selection.
        h = shardd.health()
        print(
            f"pinned edge: {h.edge_id} (region {h.region}, sync_gap {h.sync_gap})"
        )


if __name__ == "__main__":
    main()
