package xyz.shardd.sdk

val DEFAULT_EDGES: List<String> =
    listOf(
        "https://use1.api.shardd.xyz",
        "https://euc1.api.shardd.xyz",
        "https://ape1.api.shardd.xyz",
    )

private const val MAX_ACCEPTABLE_SYNC_GAP = 100L
private const val COOLDOWN_MS = 60_000L
internal const val PROBE_TIMEOUT_MS = 2_000L

private data class Candidate(
    val baseUrl: String,
    var rttMs: Long? = null,
    var cooldownUntilMs: Long? = null,
)

internal data class ProbeResult(
    val baseUrl: String,
    val rttMs: Long?,
    val ok: Boolean,
)

internal class EdgeSelector(
    bootstrap: List<String>,
) {
    private val candidates = bootstrap.map { Candidate(it) }.toMutableList()
    private var initialized = false

    @Synchronized
    fun liveUrls(): List<String> {
        val now = System.currentTimeMillis()
        return candidates
            .filter { it.cooldownUntilMs == null || it.cooldownUntilMs!! <= now }
            .map { it.baseUrl }
    }

    @Synchronized
    fun needsProbe(): Boolean {
        if (!initialized) {
            return true
        }
        val now = System.currentTimeMillis()
        return candidates.none { it.cooldownUntilMs == null || it.cooldownUntilMs!! <= now }
    }

    @Synchronized
    fun markFailure(baseUrl: String) {
        val until = System.currentTimeMillis() + COOLDOWN_MS
        candidates.forEach { candidate ->
            if (candidate.baseUrl == baseUrl) {
                candidate.cooldownUntilMs = until
            }
        }
    }

    @Synchronized
    fun markSuccess(baseUrl: String) {
        candidates.forEach { candidate ->
            if (candidate.baseUrl == baseUrl) {
                candidate.cooldownUntilMs = null
            }
        }
    }

    @Synchronized
    fun bootstrapUrls(): List<String> = candidates.map { it.baseUrl }

    @Synchronized
    fun applyProbeResults(results: List<ProbeResult>) {
        val now = System.currentTimeMillis()
        results.forEach { result ->
            candidates.firstOrNull { it.baseUrl == result.baseUrl }?.let { candidate ->
                if (result.ok) {
                    candidate.rttMs = result.rttMs
                    candidate.cooldownUntilMs = null
                } else {
                    candidate.rttMs = null
                }
            }
        }
        candidates.sortWith(
            compareBy<Candidate> {
                if (it.cooldownUntilMs != null && it.cooldownUntilMs!! > now) {
                    1
                } else {
                    0
                }
            }.thenBy { it.rttMs ?: Long.MAX_VALUE },
        )
        initialized = true
    }
}

internal fun isSelectable(health: EdgeHealth): Boolean {
    if (!health.ready) {
        return false
    }
    if (health.overloaded == true) {
        return false
    }
    if (health.syncGap != null && health.syncGap > MAX_ACCEPTABLE_SYNC_GAP) {
        return false
    }
    return true
}

internal fun trimTrailingSlash(url: String): String = url.trimEnd('/')
