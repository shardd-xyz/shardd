package xyz.shardd.sdk

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Assumptions.assumeTrue
import org.junit.jupiter.api.Test

class FailoverTest {
    @Test
    fun allHealthyProbePicksOneAndWritesSucceed() {
        val gateways = gateways()
        assumeTrue(gateways.isNotEmpty(), "set SHARDD_FAILOVER_GATEWAYS to run")

        val client = Client("local-dev", ClientOptions(edges = gateways))
        val first =
            client.createEvent(
                bucket = bucket(),
                account = "alice",
                amount = 10,
                opts = CreateEventOptions(note = "failover test: phase A"),
            )
        assertFalse(first.deduplicated)

        val replay =
            client.createEvent(
                bucket = bucket(),
                account = "alice",
                amount = 10,
                opts = CreateEventOptions(idempotencyNonce = first.event.idempotencyNonce),
            )
        assertEquals(first.event.eventId, replay.event.eventId)
        assertTrue(replay.deduplicated)
    }

    @Test
    fun closedPortMixedInIsSkippedByProbe() {
        val gateways = gateways()
        assumeTrue(gateways.isNotEmpty(), "set SHARDD_FAILOVER_GATEWAYS to run")

        val client =
            Client(
                "local-dev",
                ClientOptions(edges = listOf("http://127.0.0.1:1") + gateways),
            )
        val result =
            client.createEvent(
                bucket = bucket(),
                account = "bob",
                amount = 5,
                opts = CreateEventOptions(note = "failover test: phase B"),
            )
        assertFalse(result.deduplicated)
    }

    @Test
    fun singleSurvivorStillSucceeds() {
        val gateways = gateways()
        assumeTrue(gateways.isNotEmpty(), "set SHARDD_FAILOVER_GATEWAYS to run")

        val edges = listOf("http://127.0.0.1:1", "http://127.0.0.1:2", gateways.first())
        val client = Client("local-dev", ClientOptions(edges = edges))
        val result =
            client.createEvent(
                bucket = bucket(),
                account = "carol",
                amount = 7,
                opts = CreateEventOptions(note = "failover test: phase C"),
            )
        assertFalse(result.deduplicated)
    }

    @Test
    fun midTestOutageDoesNotBreakWrites() {
        assumeTrue(System.getenv("SHARDD_FAILOVER_KILLED_GATEWAY") != null, "set SHARDD_FAILOVER_KILLED_GATEWAY to run")
        val gateways = gateways()
        assumeTrue(gateways.isNotEmpty(), "set SHARDD_FAILOVER_GATEWAYS to run")

        val client = Client("local-dev", ClientOptions(edges = gateways))
        val result =
            client.createEvent(
                bucket = bucket(),
                account = "dan",
                amount = 3,
                opts = CreateEventOptions(note = "failover test: phase D - mid-outage"),
            )
        assertFalse(result.deduplicated)
    }

    private fun bucket(): String = System.getenv("SHARDD_FAILOVER_BUCKET") ?: "failover-test"

    private fun gateways(): List<String> =
        System.getenv("SHARDD_FAILOVER_GATEWAYS")
            ?.split(',')
            ?.map { it.trim() }
            ?.filter { it.isNotEmpty() }
            .orEmpty()
}
