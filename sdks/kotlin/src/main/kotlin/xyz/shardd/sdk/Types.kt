package xyz.shardd.sdk

data class Event(
    val eventId: String,
    val originNodeId: String,
    val originEpoch: Long,
    val originSeq: Long,
    val createdAtUnixMs: Long,
    val type: String,
    val bucket: String,
    val account: String,
    val amount: Long,
    val note: String?,
    val idempotencyNonce: String,
    val voidRef: String?,
    val holdAmount: Long,
    val holdExpiresAtUnixMs: Long,
)

data class CreateEventOptions(
    val note: String? = null,
    val idempotencyNonce: String? = null,
    val maxOverdraft: Long? = null,
    val minAcks: Int? = null,
    val ackTimeoutMs: Long? = null,
    val holdAmount: Long? = null,
    val holdExpiresAtUnixMs: Long? = null,
    val settleReservation: String? = null,
    val releaseReservation: String? = null,
)

data class CreateEventResult(
    val event: Event,
    val balance: Long,
    val availableBalance: Long,
    val deduplicated: Boolean,
    val acks: AckInfo,
    val emittedEvents: List<Event> = emptyList(),
)

data class Reservation(
    val reservationId: String,
    val expiresAtUnixMs: Long,
    val balance: Long,
    val availableBalance: Long,
)

data class AckInfo(
    val requested: Int,
    val received: Int,
    val timeout: Boolean,
)

data class EventList(
    val events: List<Event>,
)

data class AccountBalance(
    val bucket: String,
    val account: String,
    val balance: Long,
    val availableBalance: Long,
    val activeHoldTotal: Long,
    val eventCount: Long,
)

typealias AccountDetail = AccountBalance

data class Balances(
    val accounts: List<AccountBalance>,
    val totalBalance: Long,
)

data class EdgeInfo(
    val edgeId: String,
    val region: String,
    val baseUrl: String,
    val ready: Boolean,
    val reachable: Boolean,
    val syncGap: Long?,
    val overloaded: Boolean?,
    val healthyNodes: Int,
    val discoveredNodes: Int,
    val bestNodeRttMs: Long?,
)

data class EdgeHealth(
    val edgeId: String?,
    val region: String?,
    val baseUrl: String?,
    val ready: Boolean,
    val discoveredNodes: Int,
    val healthyNodes: Int,
    val bestNodeRttMs: Long?,
    val syncGap: Long?,
    val overloaded: Boolean?,
    val authEnabled: Boolean,
)

internal data class EdgeDirectoryResponse(
    val edges: List<EdgeInfo>,
)

internal data class CreateEventBody(
    val bucket: String,
    val account: String,
    val amount: Long,
    val idempotencyNonce: String,
    val note: String? = null,
    val maxOverdraft: Long? = null,
    val minAcks: Int? = null,
    val ackTimeoutMs: Long? = null,
    val holdAmount: Long? = null,
    val holdExpiresAtUnixMs: Long? = null,
    val settleReservation: String? = null,
    val releaseReservation: String? = null,
)

internal data class GatewayErrorBody(
    val error: String? = null,
    val message: String? = null,
    val balance: Long? = null,
    val availableBalance: Long? = null,
    val limit: Long? = null,
)
