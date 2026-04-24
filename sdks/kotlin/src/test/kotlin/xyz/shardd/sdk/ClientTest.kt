package xyz.shardd.sdk

import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Test

class ClientTest {
    private val server = MockWebServer()

    @AfterEach
    fun tearDown() {
        server.shutdown()
    }

    @Test
    fun createEventReturnsResult() {
        server.dispatcher =
            dispatcher(
                healthResponse = healthOk(server.url("/").toString().trimEnd('/')),
                eventsResponse =
                    """
                    {
                      "event": {
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
                        "void_ref": null,
                        "hold_amount": 0,
                        "hold_expires_at_unix_ms": 0
                      },
                      "balance": 500,
                      "available_balance": 500,
                      "deduplicated": false,
                      "acks": { "requested": 1, "received": 1, "timeout": false }
                    }
                    """.trimIndent(),
                eventsStatus = 201,
            )
        val client = Client("test-key", ClientOptions(edges = listOf(server.url("/").toString())))
        val result = client.createEvent("demo", "alice", 500)

        assertEquals("evt-1", result.event.eventId)
        assertEquals(500, result.balance)
        assertFalse(result.deduplicated)
    }

    @Test
    fun insufficientFundsIsTyped() {
        server.dispatcher =
            dispatcher(
                healthResponse = healthOk(server.url("/").toString().trimEnd('/')),
                eventsResponse =
                    """
                    {
                      "error": "insufficient funds",
                      "balance": 10,
                      "available_balance": 10,
                      "limit": 0
                    }
                    """.trimIndent(),
                eventsStatus = 422,
            )
        val client = Client("test-key", ClientOptions(edges = listOf(server.url("/").toString())))
        val error =
            assertThrows(InsufficientFundsError::class.java) {
                client.createEvent("demo", "alice", -100)
            }

        assertEquals(10, error.balance)
        assertEquals(10, error.availableBalance)
    }

    private fun dispatcher(
        healthResponse: String,
        eventsResponse: String,
        eventsStatus: Int,
    ): Dispatcher =
        object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse =
                when (request.path?.substringBefore('?')) {
                    "/gateway/health" ->
                        MockResponse()
                            .setResponseCode(200)
                            .setBody(healthResponse)
                    "/events" ->
                        MockResponse()
                            .setResponseCode(eventsStatus)
                            .setBody(eventsResponse)
                    else ->
                        MockResponse()
                            .setResponseCode(500)
                            .setBody("""{"error":"unmocked path ${request.path}"}""")
                }
        }

    private fun healthOk(baseUrl: String): String =
        """
        {
          "edge_id": "use1",
          "region": "us-east-1",
          "base_url": "$baseUrl",
          "ready": true,
          "discovered_nodes": 3,
          "healthy_nodes": 3,
          "best_node_rtt_ms": 5,
          "sync_gap": 0,
          "overloaded": false,
          "auth_enabled": true
        }
        """.trimIndent()
}
