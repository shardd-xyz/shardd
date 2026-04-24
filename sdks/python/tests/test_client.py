"""Unit tests against a mocked httpx transport — no live gateway needed."""
from __future__ import annotations

import pytest
import httpx

from shardd import InsufficientFundsError, Shardd

HEALTH_OK = {
    "edge_id": "use1",
    "region": "us-east-1",
    "base_url": "https://use1.api.shardd.xyz",
    "ready": True,
    "discovered_nodes": 3,
    "healthy_nodes": 3,
    "best_node_rtt_ms": 5,
    "sync_gap": 0,
    "overloaded": False,
    "auth_enabled": True,
}

EVENT_SAMPLE = {
    "event_id": "evt-1",
    "origin_node_id": "n1",
    "origin_epoch": 1,
    "origin_seq": 42,
    "created_at_unix_ms": 1700000000000,
    "type": "standard",
    "bucket": "demo",
    "account": "alice",
    "amount": 500,
    "note": "test",
    "idempotency_nonce": "nonce-1",
    "void_ref": None,
    "hold_amount": 0,
    "hold_expires_at_unix_ms": 0,
}


def _handler(responses: dict[str, httpx.Response]):
    def handler(request: httpx.Request) -> httpx.Response:
        for key, resp in responses.items():
            if key in str(request.url):
                return resp
        return httpx.Response(500, json={"error": f"unmocked URL {request.url}"})

    return handler


def test_create_event_returns_result():
    transport = httpx.MockTransport(
        _handler(
            {
                "/gateway/health": httpx.Response(200, json=HEALTH_OK),
                "/events": httpx.Response(
                    201,
                    json={
                        "event": EVENT_SAMPLE,
                        "balance": 500,
                        "available_balance": 500,
                        "deduplicated": False,
                        "acks": {"requested": 1, "received": 1, "timeout": False},
                    },
                ),
            }
        )
    )
    http = httpx.Client(transport=transport)
    shardd = Shardd("test-key", http=http)

    result = shardd.create_event("demo", "alice", 500)
    assert result.event.event_id == "evt-1"
    assert result.balance == 500
    assert result.deduplicated is False


def test_insufficient_funds_is_typed():
    transport = httpx.MockTransport(
        _handler(
            {
                "/gateway/health": httpx.Response(200, json=HEALTH_OK),
                "/events": httpx.Response(
                    422,
                    json={
                        "error": "insufficient funds",
                        "balance": 10,
                        "available_balance": 10,
                        "limit": 0,
                    },
                ),
            }
        )
    )
    http = httpx.Client(transport=transport)
    shardd = Shardd("test-key", http=http)

    with pytest.raises(InsufficientFundsError) as exc:
        shardd.create_event("demo", "alice", -100)
    assert exc.value.balance == 10
    assert exc.value.available_balance == 10
