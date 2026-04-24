"""Edge-selection + failover logic shared by Shardd (sync) and
AsyncShardd (async)."""
from __future__ import annotations

import time
from dataclasses import dataclass, field
from threading import Lock
from typing import Optional

from .errors import ServiceUnavailableError
from .types import EdgeHealth

DEFAULT_EDGES = [
    "https://use1.api.shardd.xyz",
    "https://euc1.api.shardd.xyz",
    "https://ape1.api.shardd.xyz",
]

MAX_ACCEPTABLE_SYNC_GAP = 100
COOLDOWN_MS = 60_000
PROBE_TIMEOUT_S = 2.0


@dataclass
class _Candidate:
    base_url: str
    rtt_ms: Optional[int] = None
    cooldown_until_ms: Optional[int] = None


class EdgeSelector:
    """Keeps the ranked candidate list and applies failover/cooldown
    policy. Thread-safe for shared use by the sync client; the async
    client uses its own asyncio lock on top of the same state."""

    def __init__(self, bootstrap: list[str]) -> None:
        self._lock = Lock()
        self._candidates: list[_Candidate] = [_Candidate(u) for u in bootstrap]
        self._initialized = False

    def live_urls(self) -> list[str]:
        now = _now_ms()
        with self._lock:
            return [
                c.base_url
                for c in self._candidates
                if c.cooldown_until_ms is None or c.cooldown_until_ms <= now
            ]

    def needs_probe(self) -> bool:
        if not self._initialized:
            return True
        now = _now_ms()
        with self._lock:
            return not any(
                c.cooldown_until_ms is None or c.cooldown_until_ms <= now
                for c in self._candidates
            )

    def mark_failure(self, base_url: str) -> None:
        until = _now_ms() + COOLDOWN_MS
        with self._lock:
            for c in self._candidates:
                if c.base_url == base_url:
                    c.cooldown_until_ms = until

    def mark_success(self, base_url: str) -> None:
        with self._lock:
            for c in self._candidates:
                if c.base_url == base_url:
                    c.cooldown_until_ms = None

    def bootstrap_urls(self) -> list[str]:
        with self._lock:
            return [c.base_url for c in self._candidates]

    def apply_probe_results(
        self,
        results: list[tuple[str, Optional[int], bool]],
    ) -> None:
        """Each tuple: ``(base_url, rtt_ms or None, ok)``.

        A probe is a *weak* signal: the gateway's mesh client refresh
        can briefly report ``healthy_nodes: 0`` / ``ready: false`` on
        an otherwise-fine edge, and cooling it for 60s would starve
        the next request for no reason. So probes only re-rank —
        real-request failures (503/504/timeout/network) open cooldowns.
        """
        now = _now_ms()
        with self._lock:
            for base_url, rtt_ms, ok in results:
                for c in self._candidates:
                    if c.base_url != base_url:
                        continue
                    if ok:
                        c.rtt_ms = rtt_ms
                        # Successful probe clears any prior request-level
                        # cooldown — the edge is observed healthy now.
                        c.cooldown_until_ms = None
                    else:
                        c.rtt_ms = None
                        # Deliberately no cooldown on probe failure.

            def sort_key(c: _Candidate) -> tuple[int, int]:
                cool = (
                    1
                    if c.cooldown_until_ms and c.cooldown_until_ms > now
                    else 0
                )
                return (cool, c.rtt_ms if c.rtt_ms is not None else 10**9)

            self._candidates.sort(key=sort_key)
            self._initialized = True


def is_selectable(health: EdgeHealth) -> bool:
    if not health.ready:
        return False
    if health.overloaded is True:
        return False
    if (
        health.sync_gap is not None
        and health.sync_gap > MAX_ACCEPTABLE_SYNC_GAP
    ):
        return False
    return True


def _now_ms() -> int:
    return int(time.time() * 1000)


def ensure_nonempty(urls: list[str]) -> None:
    if not urls:
        raise ServiceUnavailableError("all edges unhealthy")
