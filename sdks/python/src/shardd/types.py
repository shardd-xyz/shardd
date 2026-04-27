"""Data types returned and accepted by the shardd client."""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Optional


@dataclass
class Event:
    """One immutable ledger entry."""

    event_id: str
    origin_node_id: str
    origin_epoch: int
    origin_seq: int
    created_at_unix_ms: int
    type: str
    bucket: str
    account: str
    amount: int
    note: Optional[str]
    idempotency_nonce: str
    void_ref: Optional[str]
    hold_amount: int
    hold_expires_at_unix_ms: int

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "Event":
        return cls(
            event_id=d["event_id"],
            origin_node_id=d["origin_node_id"],
            origin_epoch=int(d.get("origin_epoch", 1)),
            origin_seq=int(d["origin_seq"]),
            created_at_unix_ms=int(d["created_at_unix_ms"]),
            type=d.get("type", "standard"),
            bucket=d["bucket"],
            account=d["account"],
            amount=int(d["amount"]),
            note=d.get("note"),
            idempotency_nonce=d["idempotency_nonce"],
            void_ref=d.get("void_ref"),
            hold_amount=int(d.get("hold_amount", 0)),
            hold_expires_at_unix_ms=int(d.get("hold_expires_at_unix_ms", 0)),
        )


@dataclass
class AckInfo:
    requested: int = 0
    received: int = 0
    timeout: bool = False

    @classmethod
    def from_dict(cls, d: Optional[dict[str, Any]]) -> "AckInfo":
        if not d:
            return cls()
        return cls(
            requested=int(d.get("requested", 0)),
            received=int(d.get("received", 0)),
            timeout=bool(d.get("timeout", False)),
        )


@dataclass
class CreateEventResult:
    event: Event
    balance: int
    available_balance: int
    deduplicated: bool
    acks: AckInfo
    emitted_events: list[Event] = field(default_factory=list)

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "CreateEventResult":
        return cls(
            event=Event.from_dict(d["event"]),
            balance=int(d["balance"]),
            available_balance=int(d.get("available_balance", d["balance"])),
            deduplicated=bool(d.get("deduplicated", False)),
            acks=AckInfo.from_dict(d.get("acks")),
            emitted_events=[Event.from_dict(e) for e in d.get("emitted_events", [])],
        )


@dataclass
class Reservation:
    """Handle returned by ``Shardd.reserve``. Pass ``reservation_id``
    to ``settle()`` for one-shot capture or ``release()`` to cancel."""

    reservation_id: str
    expires_at_unix_ms: int
    balance: int
    available_balance: int


@dataclass
class AccountBalance:
    bucket: str
    account: str
    balance: int
    available_balance: int = 0
    active_hold_total: int = 0
    event_count: int = 0

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "AccountBalance":
        return cls(
            bucket=d["bucket"],
            account=d["account"],
            balance=int(d["balance"]),
            available_balance=int(d.get("available_balance", d["balance"])),
            active_hold_total=int(d.get("active_hold_total", 0)),
            event_count=int(d.get("event_count", 0)),
        )


@dataclass
class Balances:
    accounts: list[AccountBalance] = field(default_factory=list)
    total_balance: int = 0

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "Balances":
        return cls(
            accounts=[AccountBalance.from_dict(x) for x in d.get("accounts", [])],
            total_balance=int(d.get("total_balance", 0)),
        )


# `/collapsed/:bucket/:account` returns the same shape as a row in
# AccountBalance.
AccountDetail = AccountBalance


@dataclass
class EventList:
    events: list[Event]

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "EventList":
        return cls(events=[Event.from_dict(x) for x in d.get("events", [])])


@dataclass
class EdgeInfo:
    edge_id: str
    region: str
    base_url: str
    ready: bool = False
    reachable: bool = False
    sync_gap: Optional[int] = None
    overloaded: Optional[bool] = None
    healthy_nodes: int = 0
    discovered_nodes: int = 0
    best_node_rtt_ms: Optional[int] = None

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "EdgeInfo":
        return cls(
            edge_id=d["edge_id"],
            region=d["region"],
            base_url=d["base_url"],
            ready=bool(d.get("ready", False)),
            reachable=bool(d.get("reachable", False)),
            sync_gap=d.get("sync_gap"),
            overloaded=d.get("overloaded"),
            healthy_nodes=int(d.get("healthy_nodes", 0)),
            discovered_nodes=int(d.get("discovered_nodes", 0)),
            best_node_rtt_ms=d.get("best_node_rtt_ms"),
        )


@dataclass
class EdgeHealth:
    edge_id: Optional[str]
    region: Optional[str]
    base_url: Optional[str]
    ready: bool
    discovered_nodes: int
    healthy_nodes: int
    best_node_rtt_ms: Optional[int]
    sync_gap: Optional[int]
    overloaded: Optional[bool]
    auth_enabled: bool

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "EdgeHealth":
        return cls(
            edge_id=d.get("edge_id"),
            region=d.get("region"),
            base_url=d.get("base_url"),
            ready=bool(d.get("ready", False)),
            discovered_nodes=int(d.get("discovered_nodes", 0)),
            healthy_nodes=int(d.get("healthy_nodes", 0)),
            best_node_rtt_ms=d.get("best_node_rtt_ms"),
            sync_gap=d.get("sync_gap"),
            overloaded=d.get("overloaded"),
            auth_enabled=bool(d.get("auth_enabled", False)),
        )
