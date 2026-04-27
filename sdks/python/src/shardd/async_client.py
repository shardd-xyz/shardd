"""Asynchronous Shardd client built on httpx.AsyncClient."""
from __future__ import annotations

import asyncio
import time
import uuid
from typing import Any, Optional

import httpx

from . import _core
from .errors import (
    NetworkError,
    ServiceUnavailableError,
    ShardError,
    Timeout,
    from_status,
)
from .types import (
    AccountDetail,
    Balances,
    CreateEventResult,
    EdgeHealth,
    EdgeInfo,
    EventList,
    Reservation,
)

_DEFAULT_TIMEOUT = 30.0


class AsyncShardd:
    """Asynchronous client. Use as an async context manager for
    deterministic connection teardown."""

    def __init__(
        self,
        api_key: str,
        *,
        edges: Optional[list[str]] = None,
        timeout_s: float = _DEFAULT_TIMEOUT,
        http: Optional[httpx.AsyncClient] = None,
    ) -> None:
        if not api_key or not api_key.strip():
            raise ValueError("api_key is required")
        self._api_key = api_key
        self._selector = _core.EdgeSelector(edges or _core.DEFAULT_EDGES)
        self._http = http or httpx.AsyncClient(
            timeout=timeout_s,
            headers={"User-Agent": "shardd-python-async/0.1"},
        )
        self._owns_http = http is None

    async def aclose(self) -> None:
        if self._owns_http:
            await self._http.aclose()

    async def __aenter__(self) -> "AsyncShardd":
        return self

    async def __aexit__(self, *_exc: Any) -> None:
        await self.aclose()

    # ── public ────────────────────────────────────────────────────

    async def create_event(
        self,
        bucket: str,
        account: str,
        amount: int,
        *,
        note: Optional[str] = None,
        idempotency_nonce: Optional[str] = None,
        max_overdraft: Optional[int] = None,
        min_acks: Optional[int] = None,
        ack_timeout_ms: Optional[int] = None,
        hold_amount: Optional[int] = None,
        hold_expires_at_unix_ms: Optional[int] = None,
        settle_reservation: Optional[str] = None,
        release_reservation: Optional[str] = None,
    ) -> CreateEventResult:
        nonce = idempotency_nonce or str(uuid.uuid4())
        body: dict[str, Any] = {
            "bucket": bucket,
            "account": account,
            "amount": amount,
            "idempotency_nonce": nonce,
        }
        if note is not None:
            body["note"] = note
        if max_overdraft is not None:
            body["max_overdraft"] = max_overdraft
        if min_acks is not None:
            body["min_acks"] = min_acks
        if ack_timeout_ms is not None:
            body["ack_timeout_ms"] = ack_timeout_ms
        if hold_amount is not None:
            body["hold_amount"] = hold_amount
        if hold_expires_at_unix_ms is not None:
            body["hold_expires_at_unix_ms"] = hold_expires_at_unix_ms
        if settle_reservation is not None:
            body["settle_reservation"] = settle_reservation
        if release_reservation is not None:
            body["release_reservation"] = release_reservation
        data = await self._request("POST", "/events", json=body)
        return CreateEventResult.from_dict(data)

    async def charge(
        self,
        bucket: str,
        account: str,
        amount: int,
        **kwargs: Any,
    ) -> CreateEventResult:
        return await self.create_event(bucket, account, -abs(amount), **kwargs)

    async def credit(
        self,
        bucket: str,
        account: str,
        amount: int,
        **kwargs: Any,
    ) -> CreateEventResult:
        return await self.create_event(bucket, account, abs(amount), **kwargs)

    async def reserve(
        self,
        bucket: str,
        account: str,
        amount: int,
        ttl_ms: int,
        **kwargs: Any,
    ) -> "Reservation":
        if amount <= 0:
            raise ValueError("reserve amount must be > 0")
        if ttl_ms <= 0:
            raise ValueError("reserve ttl_ms must be > 0")
        expires_at = int(time.time() * 1000) + ttl_ms
        result = await self.create_event(
            bucket,
            account,
            0,
            hold_amount=amount,
            hold_expires_at_unix_ms=expires_at,
            **kwargs,
        )
        return Reservation(
            reservation_id=result.event.event_id,
            expires_at_unix_ms=result.event.hold_expires_at_unix_ms,
            balance=result.balance,
            available_balance=result.available_balance,
        )

    async def settle(
        self,
        bucket: str,
        account: str,
        reservation_id: str,
        amount: int,
        **kwargs: Any,
    ) -> CreateEventResult:
        return await self.create_event(
            bucket,
            account,
            -abs(amount),
            settle_reservation=reservation_id,
            **kwargs,
        )

    async def release(
        self,
        bucket: str,
        account: str,
        reservation_id: str,
        **kwargs: Any,
    ) -> CreateEventResult:
        return await self.create_event(
            bucket,
            account,
            0,
            release_reservation=reservation_id,
            **kwargs,
        )

    async def list_events(self, bucket: str) -> EventList:
        data = await self._request("GET", "/events", params={"bucket": bucket})
        return EventList.from_dict(data)

    async def get_balances(self, bucket: str) -> Balances:
        data = await self._request("GET", "/balances", params={"bucket": bucket})
        return Balances.from_dict(data)

    async def get_account(self, bucket: str, account: str) -> AccountDetail:
        from urllib.parse import quote

        data = await self._request(
            "GET", f"/collapsed/{quote(bucket, safe='')}/{quote(account, safe='')}"
        )
        return AccountDetail.from_dict(data)

    async def edges(self) -> list[EdgeInfo]:
        await self._ensure_probed()
        live = self._selector.live_urls()
        if not live:
            raise ServiceUnavailableError("no healthy edges")
        resp = await self._http.get(f"{_trim(live[0])}/gateway/edges")
        resp.raise_for_status()
        raw = resp.json()
        return [EdgeInfo.from_dict(x) for x in raw.get("edges", [])]

    async def health(self, base_url: Optional[str] = None) -> EdgeHealth:
        target = base_url
        if target is None:
            await self._ensure_probed()
            live = self._selector.live_urls()
            if not live:
                raise ServiceUnavailableError("no healthy edges")
            target = live[0]
        resp = await self._http.get(f"{_trim(target)}/gateway/health")
        if resp.status_code >= 400:
            raise from_status(resp.status_code, _maybe_json(resp))
        return EdgeHealth.from_dict(resp.json())

    # ── internal ──────────────────────────────────────────────────

    async def _ensure_probed(self) -> None:
        if self._selector.needs_probe():
            await self._probe_all()

    async def _probe_all(self) -> None:
        async def probe_one(url: str) -> tuple[str, Optional[int], bool]:
            start = time.monotonic()
            try:
                resp = await self._http.get(
                    f"{_trim(url)}/gateway/health",
                    timeout=_core.PROBE_TIMEOUT_S,
                )
                if resp.status_code != 200:
                    return (url, None, False)
                health = EdgeHealth.from_dict(resp.json())
                if not _core.is_selectable(health):
                    return (url, None, False)
                rtt = int((time.monotonic() - start) * 1000)
                return (url, rtt, True)
            except Exception:
                return (url, None, False)

        urls = self._selector.bootstrap_urls()
        results = await asyncio.gather(*(probe_one(u) for u in urls))
        self._selector.apply_probe_results(list(results))

    async def _request(
        self,
        method: str,
        path: str,
        *,
        params: Optional[dict[str, str]] = None,
        json: Optional[dict[str, Any]] = None,
    ) -> dict[str, Any]:
        await self._ensure_probed()
        urls = self._selector.live_urls()
        if not urls:
            await self._probe_all()
            urls = self._selector.live_urls()
        _core.ensure_nonempty(urls)

        headers = {
            "Authorization": f"Bearer {self._api_key}",
            "Content-Type": "application/json",
        }
        # Try candidates in priority order, capped at 3 — matches our
        # current prod topology.
        last_err: Optional[ShardError] = None
        for base in urls[:3]:
            url = f"{_trim(base)}{path}"
            try:
                resp = await self._http.request(
                    method, url, headers=headers, params=params, json=json
                )
            except httpx.TimeoutException:
                self._selector.mark_failure(base)
                last_err = Timeout()
                continue
            except httpx.HTTPError as e:
                self._selector.mark_failure(base)
                last_err = NetworkError(str(e))
                continue

            if 200 <= resp.status_code < 300:
                self._selector.mark_success(base)
                return resp.json()
            err = from_status(resp.status_code, _maybe_json(resp))
            if not err.retryable:
                raise err
            self._selector.mark_failure(base)
            last_err = err

        raise last_err or ServiceUnavailableError(
            "failover exhausted with no error captured"
        )


def _trim(s: str) -> str:
    return s[:-1] if s.endswith("/") else s


def _maybe_json(resp: httpx.Response) -> Optional[dict[str, Any]]:
    try:
        data = resp.json()
    except Exception:
        return None
    return data if isinstance(data, dict) else None
