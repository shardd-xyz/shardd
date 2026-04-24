package xyz.shardd.sdk

open class ShardError(
    val code: String,
    message: String,
) : RuntimeException(message) {
    val retryable: Boolean
        get() = code == "service_unavailable" || code == "timeout" || code == "network"
}

class InvalidInputError(message: String) : ShardError("invalid_input", message)

class UnauthorizedError(message: String) : ShardError("unauthorized", message)

class ForbiddenError(message: String) : ShardError("forbidden", message)

class NotFoundError(message: String) : ShardError("not_found", message)

class InsufficientFundsError(
    val balance: Long,
    val availableBalance: Long,
    val limit: Long,
) : ShardError(
        code = "insufficient_funds",
        message = "insufficient funds: balance=$balance, available=$availableBalance",
    )

class PaymentRequiredError : ShardError("payment_required", "payment required")

class ServiceUnavailableError(message: String) : ShardError("service_unavailable", message)

class TimeoutError : ShardError("timeout", "request timed out")

class NetworkError(message: String) : ShardError("network", message)

class DecodeError(message: String) : ShardError("decode", message)

internal fun fromStatus(
    status: Int,
    body: GatewayErrorBody? = null,
): ShardError {
    val text = body?.error ?: body?.message ?: "HTTP $status"
    return when (status) {
        400 -> InvalidInputError(text)
        401 -> UnauthorizedError(text)
        402 -> PaymentRequiredError()
        403 -> ForbiddenError(text)
        404 -> NotFoundError(text)
        408, 504 -> TimeoutError()
        422 -> InsufficientFundsError(
            balance = body?.balance ?: 0,
            availableBalance = body?.availableBalance ?: 0,
            limit = body?.limit ?: 0,
        )
        503 -> ServiceUnavailableError(text)
        else -> DecodeError("unexpected HTTP $status: $text")
    }
}
