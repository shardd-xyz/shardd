declare const process: {
  env: Record<string, string | undefined>;
  argv: string[];
  exit(code?: number): never;
};

type EdgeDirectoryResponse = {
  observed_at_unix_ms: number;
  edges: PublicEdgeSummary[];
};

type PublicEdgeSummary = {
  edge_id: string;
  region: string;
  base_url: string;
  health_url: string;
  reachable: boolean;
  ready: boolean;
  observed_at_unix_ms?: number;
  discovered_nodes?: number;
  healthy_nodes?: number;
  best_node_rtt_ms?: number;
  sync_gap?: number;
  overloaded?: boolean;
};

type PublicEdgeHealthResponse = {
  observed_at_unix_ms: number;
  edge_id?: string;
  region?: string;
  base_url?: string;
  ready: boolean;
  discovered_nodes: number;
  healthy_nodes: number;
  best_node_rtt_ms?: number;
  sync_gap?: number;
  overloaded?: boolean;
  auth_enabled: boolean;
};

type Candidate = {
  edgeId: string;
  region: string;
  baseUrl: string;
  latencyMs: number;
  ready: boolean;
  overloaded: boolean;
  healthyNodes: number;
};

function normalizeBaseUrl(url: string): string {
  return url.replace(/\/+$/, "");
}

async function fetchJson<T>(
  url: string,
  init?: RequestInit,
): Promise<{ payload: T; latencyMs: number }> {
  const started = Date.now();
  const response = await fetch(url, init);
  const latencyMs = Date.now() - started;
  if (!response.ok) {
    throw new Error(`${url} -> ${response.status}`);
  }
  return {
    payload: (await response.json()) as T,
    latencyMs,
  };
}

async function discoverEdges(bootstraps: string[]): Promise<PublicEdgeSummary[]> {
  const seen = new Map<string, PublicEdgeSummary>();
  for (const rawBaseUrl of bootstraps) {
    const baseUrl = normalizeBaseUrl(rawBaseUrl);
    seen.set(baseUrl, {
      edge_id: baseUrl,
      region: "unknown",
      base_url: baseUrl,
      health_url: `${baseUrl}/gateway/health`,
      reachable: false,
      ready: false,
    });
    try {
      const { payload } = await fetchJson<EdgeDirectoryResponse>(`${baseUrl}/gateway/edges`);
      for (const edge of payload.edges) {
        seen.set(normalizeBaseUrl(edge.base_url), edge);
      }
    } catch {
      continue;
    }
  }
  return [...seen.values()];
}

async function probeEdge(edge: PublicEdgeSummary): Promise<Candidate | null> {
  const baseUrl = normalizeBaseUrl(edge.base_url);
  try {
    const { payload, latencyMs } = await fetchJson<PublicEdgeHealthResponse>(
      `${baseUrl}/gateway/health`,
    );
    return {
      edgeId: payload.edge_id ?? edge.edge_id ?? baseUrl,
      region: payload.region ?? edge.region ?? "unknown",
      baseUrl,
      latencyMs,
      ready: payload.ready,
      overloaded: Boolean(payload.overloaded),
      healthyNodes: payload.healthy_nodes,
    };
  } catch {
    return null;
  }
}

function compareCandidates(a: Candidate, b: Candidate): number {
  if (a.ready !== b.ready) {
    return a.ready ? -1 : 1;
  }
  if (a.overloaded !== b.overloaded) {
    return a.overloaded ? 1 : -1;
  }
  if (a.healthyNodes !== b.healthyNodes) {
    return b.healthyNodes - a.healthyNodes;
  }
  return a.latencyMs - b.latencyMs;
}

async function chooseBestEdge(bootstraps: string[]): Promise<{ selected: Candidate; candidates: Candidate[] }> {
  const edges = await discoverEdges(bootstraps);
  const candidates = (await Promise.all(edges.map((edge) => probeEdge(edge)))).filter(
    (candidate): candidate is Candidate => candidate !== null,
  );
  if (candidates.length === 0) {
    throw new Error("no reachable public edges");
  }
  candidates.sort(compareCandidates);
  return { selected: candidates[0], candidates };
}

async function main(): Promise<void> {
  const apiKey = process.env.SHARDD_API_KEY;
  if (!apiKey) {
    throw new Error("set SHARDD_API_KEY");
  }

  const bootstraps = process.argv.slice(2);
  if (bootstraps.length === 0) {
    throw new Error("usage: tsx public_edge_client.ts <bootstrap-url> [more-bootstrap-urls]");
  }

  const bucket = process.env.SHARDD_BUCKET ?? "orders";

  const { selected, candidates } = await chooseBestEdge(bootstraps);
  console.log(JSON.stringify({ selected, candidates }, null, 2));

  const balancesUrl = new URL(`${selected.baseUrl}/balances`);
  balancesUrl.searchParams.set("bucket", bucket);

  const response = await fetch(balancesUrl, {
    headers: {
      Authorization: `Bearer ${apiKey}`,
      Accept: "application/json",
    },
  });
  if (!response.ok) {
    throw new Error(`balances request failed: ${response.status}`);
  }

  console.log(JSON.stringify(await response.json(), null, 2));
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
