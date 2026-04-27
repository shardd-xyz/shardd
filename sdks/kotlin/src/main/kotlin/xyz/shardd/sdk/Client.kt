package xyz.shardd.sdk

import com.fasterxml.jackson.module.kotlin.readValue
import java.io.IOException
import java.net.URI
import java.net.URLEncoder
import java.net.http.HttpClient
import java.net.http.HttpRequest
import java.net.http.HttpResponse
import java.net.http.HttpTimeoutException
import java.nio.charset.StandardCharsets
import java.time.Duration
import java.util.UUID
import java.util.concurrent.CompletableFuture

private const val DEFAULT_TIMEOUT_MS = 30_000L
private const val SDK_VERSION = "0.1.0"

data class ClientOptions(
    val edges: List<String> = DEFAULT_EDGES,
    val timeoutMillis: Long = DEFAULT_TIMEOUT_MS,
) {
    init {
        require(timeoutMillis > 0) { "timeoutMillis must be positive" }
    }
}

internal interface HttpTransport {
    fun send(request: HttpRequest): HttpResponse<String>
}

private class JavaHttpTransport(
    timeoutMillis: Long,
) : HttpTransport {
    private val client: HttpClient =
        HttpClient
            .newBuilder()
            .connectTimeout(Duration.ofMillis(timeoutMillis))
            .build()

    override fun send(request: HttpRequest): HttpResponse<String> =
        client.send(request, HttpResponse.BodyHandlers.ofString())
}

class Client private constructor(
    private val apiKey: String,
    private val options: ClientOptions,
    private val transport: HttpTransport,
    private val selector: EdgeSelector,
) {
    @JvmOverloads
    constructor(
        apiKey: String,
        options: ClientOptions = ClientOptions(),
    ) : this(
            apiKey = apiKey,
            options = options,
            transport = JavaHttpTransport(options.timeoutMillis),
            selector = EdgeSelector(options.edges),
        )

    init {
        require(apiKey.isNotBlank()) { "apiKey is required" }
    }

    internal constructor(
        apiKey: String,
        options: ClientOptions,
        transport: HttpTransport,
    ) : this(
            apiKey = apiKey,
            options = options,
            transport = transport,
            selector = EdgeSelector(options.edges),
        )

    @JvmOverloads
    fun createEvent(
        bucket: String,
        account: String,
        amount: Long,
        opts: CreateEventOptions = CreateEventOptions(),
    ): CreateEventResult {
        val nonce = opts.idempotencyNonce ?: UUID.randomUUID().toString()
        val body =
            CreateEventBody(
                bucket = bucket,
                account = account,
                amount = amount,
                idempotencyNonce = nonce,
                note = opts.note,
                maxOverdraft = opts.maxOverdraft,
                minAcks = opts.minAcks,
                ackTimeoutMs = opts.ackTimeoutMs,
                holdAmount = opts.holdAmount,
                holdExpiresAtUnixMs = opts.holdExpiresAtUnixMs,
                settleReservation = opts.settleReservation,
                releaseReservation = opts.releaseReservation,
            )
        return request("POST", "/events", body = body)
    }

    @JvmOverloads
    fun charge(
        bucket: String,
        account: String,
        amount: Long,
        opts: CreateEventOptions = CreateEventOptions(),
    ): CreateEventResult = createEvent(bucket, account, -kotlin.math.abs(amount), opts)

    @JvmOverloads
    fun credit(
        bucket: String,
        account: String,
        amount: Long,
        opts: CreateEventOptions = CreateEventOptions(),
    ): CreateEventResult = createEvent(bucket, account, kotlin.math.abs(amount), opts)

    /**
     * Reserve `amount` credit units for `ttlMs` ms. Returns a
     * [Reservation] handle whose `reservationId` you pass to [settle]
     * (one-shot capture) or [release] (cancel). If neither is called
     * before `ttlMs` elapses, the hold auto-releases passively.
     */
    @JvmOverloads
    fun reserve(
        bucket: String,
        account: String,
        amount: Long,
        ttlMs: Long,
        opts: CreateEventOptions = CreateEventOptions(),
    ): Reservation {
        require(amount > 0) { "reserve amount must be > 0" }
        require(ttlMs > 0) { "reserve ttlMs must be > 0" }
        val expiresAt = System.currentTimeMillis() + ttlMs
        val result =
            createEvent(
                bucket,
                account,
                0,
                opts.copy(
                    holdAmount = amount,
                    holdExpiresAtUnixMs = expiresAt,
                ),
            )
        return Reservation(
            reservationId = result.event.eventId,
            expiresAtUnixMs = result.event.holdExpiresAtUnixMs,
            balance = result.balance,
            availableBalance = result.availableBalance,
        )
    }

    /**
     * One-shot capture against an existing reservation. `amount` is
     * the absolute value to charge; must be ≤ the reservation's hold.
     * The server emits both the charge and a `hold_release`, returning
     * any unused remainder to available balance.
     */
    @JvmOverloads
    fun settle(
        bucket: String,
        account: String,
        reservationId: String,
        amount: Long,
        opts: CreateEventOptions = CreateEventOptions(),
    ): CreateEventResult =
        createEvent(
            bucket,
            account,
            -kotlin.math.abs(amount),
            opts.copy(settleReservation = reservationId),
        )

    /** Cancel a reservation outright — releases the entire hold, no charge. */
    @JvmOverloads
    fun release(
        bucket: String,
        account: String,
        reservationId: String,
        opts: CreateEventOptions = CreateEventOptions(),
    ): CreateEventResult =
        createEvent(
            bucket,
            account,
            0,
            opts.copy(releaseReservation = reservationId),
        )

    fun listEvents(bucket: String): EventList =
        request("GET", "/events", query = mapOf("bucket" to bucket))

    fun getBalances(bucket: String): Balances =
        request("GET", "/balances", query = mapOf("bucket" to bucket))

    fun getAccount(
        bucket: String,
        account: String,
    ): AccountDetail {
        val path = "/collapsed/${urlencode(bucket)}/${urlencode(account)}"
        return request("GET", path)
    }

    fun edges(): List<EdgeInfo> {
        ensureProbed()
        val live = selector.liveUrls()
        if (live.isEmpty()) {
            throw ServiceUnavailableError("no healthy edges")
        }
        val response = sendWithoutAuth(live.first(), "/gateway/edges")
        if (response.statusCode() !in 200..299) {
            throw ServiceUnavailableError("edges fetch returned HTTP ${response.statusCode()}")
        }
        return decode<EdgeDirectoryResponse>(response.body()).edges
    }

    @JvmOverloads
    fun health(baseUrl: String? = null): EdgeHealth {
        val target =
            baseUrl ?: run {
                ensureProbed()
                val live = selector.liveUrls()
                if (live.isEmpty()) {
                    throw ServiceUnavailableError("no healthy edges")
                }
                live.first()
            }
        val response = sendWithoutAuth(target, "/gateway/health")
        if (response.statusCode() !in 200..299) {
            throw fromStatus(response.statusCode(), decodeOrNull<GatewayErrorBody>(response.body()))
        }
        return decode(response.body())
    }

    private fun ensureProbed() {
        if (selector.needsProbe()) {
            probeAll()
        }
    }

    private fun probeAll() {
        val urls = selector.bootstrapUrls()
        if (urls.isEmpty()) {
            throw ServiceUnavailableError("no edges configured")
        }
        val results =
            urls.map { baseUrl ->
                CompletableFuture.supplyAsync {
                    val start = System.nanoTime()
                    try {
                        val response = sendWithoutAuth(baseUrl, "/gateway/health", PROBE_TIMEOUT_MS)
                        if (response.statusCode() != 200) {
                            ProbeResult(baseUrl, null, false)
                        } else {
                            val health = decode<EdgeHealth>(response.body())
                            if (isSelectable(health)) {
                                val rttMs = Duration.ofNanos(System.nanoTime() - start).toMillis()
                                ProbeResult(baseUrl, rttMs, true)
                            } else {
                                ProbeResult(baseUrl, null, false)
                            }
                        }
                    } catch (_: Exception) {
                        ProbeResult(baseUrl, null, false)
                    }
                }
            }.map { it.join() }
        selector.applyProbeResults(results)
    }

    private inline fun <reified T> request(
        method: String,
        path: String,
        query: Map<String, String> = emptyMap(),
        body: Any? = null,
    ): T {
        ensureProbed()
        var urls = selector.liveUrls()
        if (urls.isEmpty()) {
            probeAll()
            urls = selector.liveUrls()
        }
        if (urls.isEmpty()) {
            throw ServiceUnavailableError("all edges unhealthy")
        }

        val queryString =
            if (query.isEmpty()) {
                ""
            } else {
                query.entries.joinToString(
                    prefix = "?",
                    separator = "&",
                ) { (key, value) -> "${urlencode(key)}=${urlencode(value)}" }
            }

        var lastErr: ShardError? = null
        for (baseUrl in urls.take(3)) {
            try {
                val request =
                    requestBuilder("${trimTrailingSlash(baseUrl)}$path$queryString")
                        .header("Authorization", "Bearer $apiKey")
                        .apply {
                            if (body != null) {
                                header("Content-Type", "application/json")
                                method(
                                    method,
                                    HttpRequest.BodyPublishers.ofString(json.writeValueAsString(body)),
                                )
                            } else {
                                method(method, HttpRequest.BodyPublishers.noBody())
                            }
                        }.build()

                val response = transport.send(request)
                if (response.statusCode() in 200..299) {
                    selector.markSuccess(baseUrl)
                    return decode(response.body())
                }
                val err = fromStatus(response.statusCode(), decodeOrNull<GatewayErrorBody>(response.body()))
                if (!err.retryable) {
                    throw err
                }
                selector.markFailure(baseUrl)
                lastErr = err
            } catch (e: ShardError) {
                if (!e.retryable) {
                    throw e
                }
                selector.markFailure(baseUrl)
                lastErr = e
            } catch (_: HttpTimeoutException) {
                selector.markFailure(baseUrl)
                lastErr = TimeoutError()
            } catch (e: InterruptedException) {
                Thread.currentThread().interrupt()
                selector.markFailure(baseUrl)
                lastErr = NetworkError(e.message ?: "request interrupted")
            } catch (e: IOException) {
                selector.markFailure(baseUrl)
                lastErr = NetworkError(e.message ?: "network error")
            }
        }

        throw lastErr ?: ServiceUnavailableError("failover exhausted with no error captured")
    }

    private fun sendWithoutAuth(
        baseUrl: String,
        path: String,
        timeoutMillis: Long = options.timeoutMillis,
    ): HttpResponse<String> {
        val request =
            requestBuilder("${trimTrailingSlash(baseUrl)}$path", timeoutMillis)
                .GET()
                .build()
        return transport.send(request)
    }

    private fun requestBuilder(
        url: String,
        timeoutMillis: Long = options.timeoutMillis,
    ): HttpRequest.Builder =
        HttpRequest
            .newBuilder(URI.create(url))
            .timeout(Duration.ofMillis(timeoutMillis))
            .header("Accept", "application/json")
            .header("User-Agent", "shardd-kotlin/$SDK_VERSION")

    private inline fun <reified T> decode(body: String): T =
        try {
            json.readValue(body)
        } catch (e: Exception) {
            throw DecodeError(e.message ?: "failed to decode JSON")
        }

    private inline fun <reified T> decodeOrNull(body: String): T? =
        try {
            json.readValue(body)
        } catch (_: Exception) {
            null
        }
}

private fun urlencode(value: String): String =
    URLEncoder
        .encode(value, StandardCharsets.UTF_8)
        .replace("+", "%20")
