import { EdgeHealth } from "./types.js";
import { ServiceUnavailableError, ShardError } from "./errors.js";

export const DEFAULT_EDGES = [
  "https://use1.api.shardd.xyz",
  "https://euc1.api.shardd.xyz",
  "https://ape1.api.shardd.xyz",
];

const MAX_ACCEPTABLE_SYNC_GAP = 100;
const COOLDOWN_MS = 60_000;
const PROBE_TIMEOUT_MS = 2_000;

interface Candidate {
  baseUrl: string;
  rttMs: number | null;
  cooldownUntil: number | null;
}

export class EdgeSelector {
  private candidates: Candidate[];
  private initialized = false;

  constructor(bootstrap: string[]) {
    this.candidates = bootstrap.map((baseUrl) => ({
      baseUrl,
      rttMs: null,
      cooldownUntil: null,
    }));
  }

  /** Base URLs not currently in cooldown, in RTT order. */
  liveUrls(): string[] {
    const now = Date.now();
    return this.candidates
      .filter((c) => !c.cooldownUntil || c.cooldownUntil <= now)
      .map((c) => c.baseUrl);
  }

  needsProbe(): boolean {
    if (!this.initialized) return true;
    const now = Date.now();
    return !this.candidates.some(
      (c) => !c.cooldownUntil || c.cooldownUntil <= now,
    );
  }

  markFailure(baseUrl: string): void {
    const until = Date.now() + COOLDOWN_MS;
    for (const c of this.candidates) {
      if (c.baseUrl === baseUrl) c.cooldownUntil = until;
    }
  }

  markSuccess(baseUrl: string): void {
    for (const c of this.candidates) {
      if (c.baseUrl === baseUrl) c.cooldownUntil = null;
    }
  }

  /**
   * Probe every bootstrap edge in parallel and re-rank by RTT.
   *
   * A probe is a *weak* signal: the gateway's mesh client refresh
   * cycle can briefly report `healthy_nodes: 0` / `ready: false` even
   * though the edge is fine, and cooling it off for 60s would starve
   * the next request for no good reason. So probes only re-rank —
   * real-request failures (503/504/timeout/network) open cooldowns.
   */
  async probeAll(fetchImpl: typeof fetch): Promise<void> {
    if (this.candidates.length === 0) {
      throw new ServiceUnavailableError("no edges configured");
    }
    const probes = this.candidates.map(async (c) => {
      const start = Date.now();
      try {
        const health = await probeOne(fetchImpl, c.baseUrl);
        if (isSelectable(health)) {
          return { baseUrl: c.baseUrl, rttMs: Date.now() - start, ok: true };
        }
      } catch {
        // fall through to failure
      }
      return { baseUrl: c.baseUrl, rttMs: null, ok: false };
    });
    const results = await Promise.all(probes);
    const now = Date.now();
    for (const r of results) {
      const c = this.candidates.find((x) => x.baseUrl === r.baseUrl);
      if (!c) continue;
      if (r.ok) {
        c.rttMs = r.rttMs;
        c.cooldownUntil = null; // clear any prior request-level cooldown
      } else {
        c.rttMs = null;
        // Deliberately do not cool down on probe failure.
      }
    }
    this.candidates.sort((a, b) => {
      const aCool = a.cooldownUntil && a.cooldownUntil > now ? 1 : 0;
      const bCool = b.cooldownUntil && b.cooldownUntil > now ? 1 : 0;
      if (aCool !== bCool) return aCool - bCool;
      return (a.rttMs ?? Number.MAX_SAFE_INTEGER) - (b.rttMs ?? Number.MAX_SAFE_INTEGER);
    });
    this.initialized = true;
  }
}

async function probeOne(fetchImpl: typeof fetch, baseUrl: string): Promise<EdgeHealth> {
  const url = `${trimTrailingSlash(baseUrl)}/gateway/health`;
  const ac = new AbortController();
  const timer = setTimeout(() => ac.abort(), PROBE_TIMEOUT_MS);
  try {
    const resp = await fetchImpl(url, { signal: ac.signal });
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    return (await resp.json()) as EdgeHealth;
  } finally {
    clearTimeout(timer);
  }
}

function isSelectable(health: EdgeHealth): boolean {
  if (!health.ready) return false;
  if (health.overloaded === true) return false;
  if (health.sync_gap !== null && health.sync_gap > MAX_ACCEPTABLE_SYNC_GAP) {
    return false;
  }
  return true;
}

export function trimTrailingSlash(s: string): string {
  return s.endsWith("/") ? s.slice(0, -1) : s;
}

export async function fetchDirectory(
  fetchImpl: typeof fetch,
  baseUrl: string,
): Promise<{ edges: import("./types.js").EdgeInfo[] }> {
  const url = `${trimTrailingSlash(baseUrl)}/gateway/edges`;
  const resp = await fetchImpl(url);
  if (!resp.ok) {
    throw new ServiceUnavailableError(
      `edges fetch returned HTTP ${resp.status}`,
    ) as ShardError;
  }
  return (await resp.json()) as { edges: import("./types.js").EdgeInfo[] };
}
