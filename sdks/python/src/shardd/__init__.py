"""Official Python client for shardd — a globally distributed credit ledger.

The :class:`Shardd` and :class:`AsyncShardd` clients default to the three
prod regions, probe ``/gateway/health`` on first use, pin the closest
healthy edge, and fail over once per request on transient errors.

Quickstart::

    from shardd import Shardd

    shardd = Shardd(os.environ["SHARDD_API_KEY"])
    result = shardd.create_event("my-app", "user:42", 500, note="signup bonus")
    print(result.event.event_id, result.balance)
"""
from __future__ import annotations

from .async_client import AsyncShardd
from .client import Shardd
from .errors import (
    DecodeError,
    ForbiddenError,
    InsufficientFundsError,
    InvalidInputError,
    NetworkError,
    NotFoundError,
    PaymentRequiredError,
    ServiceUnavailableError,
    ShardError,
    Timeout,
    UnauthorizedError,
)
from .types import (
    AccountBalance,
    AccountDetail,
    AckInfo,
    Balances,
    CreateEventResult,
    EdgeHealth,
    EdgeInfo,
    Event,
    EventList,
    Reservation,
)

__all__ = [
    "Shardd",
    "AsyncShardd",
    # errors
    "ShardError",
    "InvalidInputError",
    "UnauthorizedError",
    "ForbiddenError",
    "NotFoundError",
    "InsufficientFundsError",
    "PaymentRequiredError",
    "ServiceUnavailableError",
    "Timeout",
    "NetworkError",
    "DecodeError",
    # types
    "AccountBalance",
    "AccountDetail",
    "AckInfo",
    "Balances",
    "CreateEventResult",
    "EdgeHealth",
    "EdgeInfo",
    "Event",
    "EventList",
    "Reservation",
]

__version__ = "0.1.0"
