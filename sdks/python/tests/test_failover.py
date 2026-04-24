"""Failover integration test.

Runs only when ``SHARDD_FAILOVER_GATEWAYS`` is set — a comma-separated
list of local gateway URLs. ``./run sdk:test:failover`` brings up the
3-gateway harness, sets the env, and invokes pytest.
"""
from __future__ import annotations

import os

import pytest

from shardd import Shardd

GATEWAYS = [
    s.strip()
    for s in os.environ.get("SHARDD_FAILOVER_GATEWAYS", "").split(",")
    if s.strip()
]
BUCKET = os.environ.get("SHARDD_FAILOVER_BUCKET", "failover-test")

pytestmark = pytest.mark.skipif(
    not GATEWAYS,
    reason="SHARDD_FAILOVER_GATEWAYS not set — run via ./run sdk:test:failover",
)


def test_all_healthy_probe_and_idempotent_replay():
    with Shardd("local-dev", edges=list(GATEWAYS)) as shardd:
        first = shardd.create_event(
            BUCKET, "alice", 10, note="failover test: phase A"
        )
        assert first.deduplicated is False

        replay = shardd.create_event(
            BUCKET,
            "alice",
            10,
            idempotency_nonce=first.event.idempotency_nonce,
        )
        assert replay.deduplicated is True
        assert replay.event.event_id == first.event.event_id


def test_closed_port_mixed_in_is_skipped():
    edges = ["http://127.0.0.1:1", *GATEWAYS]
    with Shardd("local-dev", edges=edges) as shardd:
        result = shardd.create_event(
            BUCKET, "bob", 5, note="failover test: phase B"
        )
        assert result.deduplicated is False


def test_single_survivor_still_succeeds():
    edges = ["http://127.0.0.1:1", "http://127.0.0.1:2", GATEWAYS[0]]
    with Shardd("local-dev", edges=edges) as shardd:
        result = shardd.create_event(
            BUCKET, "carol", 7, note="failover test: phase C"
        )
        assert result.deduplicated is False


@pytest.mark.skipif(
    "SHARDD_FAILOVER_KILLED_GATEWAY" not in os.environ,
    reason="only runs in the harness's kill phase",
)
def test_mid_run_outage_does_not_break_writes():
    with Shardd("local-dev", edges=list(GATEWAYS)) as shardd:
        result = shardd.create_event(
            BUCKET, "dan", 3, note="failover test: phase D — mid-outage"
        )
        assert result.deduplicated is False
