"""Exceptions raised by the shardd client. Branch on the specific
subclasses or on ``err.code`` (a string)."""
from __future__ import annotations

from typing import Any, Optional


class ShardError(Exception):
    """Base class for every SDK exception."""

    code: str = "unknown"
    retryable: bool = False

    def __init__(self, message: str) -> None:
        super().__init__(message)
        self.message = message


class InvalidInputError(ShardError):
    code = "invalid_input"


class UnauthorizedError(ShardError):
    code = "unauthorized"


class ForbiddenError(ShardError):
    code = "forbidden"


class NotFoundError(ShardError):
    code = "not_found"


class InsufficientFundsError(ShardError):
    code = "insufficient_funds"

    def __init__(self, balance: int, available_balance: int, limit: int) -> None:
        super().__init__(
            f"insufficient funds: balance={balance}, available={available_balance}"
        )
        self.balance = balance
        self.available_balance = available_balance
        self.limit = limit


class PaymentRequiredError(ShardError):
    code = "payment_required"

    def __init__(self) -> None:
        super().__init__("payment required")


class ServiceUnavailableError(ShardError):
    code = "service_unavailable"
    retryable = True


class TimeoutError_(ShardError):
    """Named with trailing underscore to avoid shadowing the stdlib."""

    code = "timeout"
    retryable = True

    def __init__(self) -> None:
        super().__init__("request timed out")


# Re-export under the conventional name — callers can still catch the
# stdlib ``TimeoutError`` for their own asyncio timeouts; our class is
# a subclass of ShardError.
Timeout = TimeoutError_


class NetworkError(ShardError):
    code = "network"
    retryable = True


class DecodeError(ShardError):
    code = "decode"


def from_status(status: int, body: Optional[dict[str, Any]] = None) -> ShardError:
    text = (
        (body or {}).get("error")
        or (body or {}).get("message")
        or f"HTTP {status}"
    )
    if status == 400:
        return InvalidInputError(text)
    if status == 401:
        return UnauthorizedError(text)
    if status == 402:
        return PaymentRequiredError()
    if status == 403:
        return ForbiddenError(text)
    if status == 404:
        return NotFoundError(text)
    if status == 422:
        b = body or {}
        return InsufficientFundsError(
            int(b.get("balance", 0)),
            int(b.get("available_balance", 0)),
            int(b.get("limit", 0)),
        )
    if status in (408, 504):
        return Timeout()
    if status == 503:
        return ServiceUnavailableError(text)
    return DecodeError(f"unexpected HTTP {status}: {text}")
