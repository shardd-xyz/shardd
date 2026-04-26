<script setup lang="ts">
import { ref, onMounted, onBeforeUnmount, computed, watch, nextTick } from "vue";

// Shape matches libs/types::PublicEdgeSummary plus browser-measured RTT.
type EdgeSummary = {
  edge_id: string;
  region: string;
  base_url: string;
  health_url?: string;
  reachable: boolean;
  ready: boolean;
  healthy_nodes?: number | null;
  best_node_rtt_ms?: number | null;
  sync_gap?: number | null;
  overloaded?: boolean | null;
  client_rtt_ms?: number | null;
};

type EdgeHealth = {
  observed_at_unix_ms: number;
  edge_id?: string | null;
  region?: string | null;
  base_url?: string | null;
  ready: boolean;
  discovered_nodes: number;
  healthy_nodes: number;
  best_node_rtt_ms?: number | null;
  sync_gap?: number | null;
  overloaded?: boolean | null;
};

// Static fallback matches the deployed topology so the SSR HTML looks
// sensible before hydration and when the live fetch fails.
const fallbackEdges: EdgeSummary[] = [
  {
    edge_id: "use1",
    region: "us-east-1",
    base_url: "https://use1.api.shardd.xyz",
    reachable: false,
    ready: false,
  },
  {
    edge_id: "euc1",
    region: "eu-central-1",
    base_url: "https://euc1.api.shardd.xyz",
    reachable: false,
    ready: false,
  },
  {
    edge_id: "ape1",
    region: "ap-east-1",
    base_url: "https://ape1.api.shardd.xyz",
    reachable: false,
    ready: false,
  },
];

const features = [
  {
    num: "01",
    title: "Credit, debit, hold",
    body: "Atomic writes with idempotency. Holds reserve balance up front with automatic expiry.",
  },
  {
    num: "02",
    title: "Write from any region",
    body: "POST to the nearest edge. Events replicate asynchronously — latency stays local.",
  },
  {
    num: "03",
    title: "No leader, no consensus",
    body: "Every node holds a full replica. Regions converge asynchronously with no coordination overhead.",
  },
  {
    num: "04",
    title: "Built for metered billing",
    body: "LLM tokens, API credits, prepaid accounts. Append-only event stream, auditable forever.",
  },
];

// Reactive state
const loaded = ref(false);
const edges = ref<EdgeSummary[]>(fallbackEdges);
const latencyHistory = ref<Record<string, number[]>>({});
const meshState = ref<"loading" | "live" | "offline">("loading");
let refetchTimer: number | undefined;
let currentAbort: AbortController | undefined;

const FETCH_TIMEOUT_MS = 3500;
const REFETCH_INTERVAL_MS = 2500;
const MAX_LATENCY_SAMPLES = 24;
const SPARKLINE_WIDTH = 56;
const SPARKLINE_HEIGHT = 16;

async function fetchEdgeHealth(
  edge: EdgeSummary,
  signal: AbortSignal,
): Promise<EdgeSummary> {
  const startedAt = window.performance.now();
  const res = await fetch(`${edge.base_url}/gateway/health`, {
    method: "GET",
    signal,
    mode: "cors",
    cache: "no-store",
    credentials: "omit",
    headers: { accept: "application/json" },
  });
  if (!res.ok) throw new Error(`${edge.base_url}: ${res.status}`);
  const health = (await res.json()) as EdgeHealth;
  const finishedAt = window.performance.now();
  return {
    edge_id: health.edge_id || edge.edge_id,
    region: health.region || edge.region,
    base_url: health.base_url || edge.base_url,
    reachable: true,
    ready: health.ready,
    healthy_nodes: health.healthy_nodes,
    best_node_rtt_ms: health.best_node_rtt_ms,
    sync_gap: health.sync_gap,
    overloaded: health.overloaded,
    client_rtt_ms: Math.max(finishedAt - startedAt, 0),
  };
}

function offlineEdge(edge: EdgeSummary): EdgeSummary {
  return {
    ...edge,
    reachable: false,
    ready: false,
    healthy_nodes: null,
    best_node_rtt_ms: null,
    sync_gap: null,
    overloaded: null,
    client_rtt_ms: null,
  };
}

function recordLatencySamples(nextEdges: EdgeSummary[]) {
  const nextHistory = { ...latencyHistory.value };
  for (const edge of nextEdges) {
    if (edge.client_rtt_ms === null || edge.client_rtt_ms === undefined) continue;
    const samples = [...(nextHistory[edge.edge_id] || []), edge.client_rtt_ms];
    nextHistory[edge.edge_id] = samples.slice(-MAX_LATENCY_SAMPLES);
  }
  latencyHistory.value = nextHistory;
}

async function refreshEdges() {
  currentAbort?.abort();
  const ctrl = new AbortController();
  currentAbort = ctrl;
  const timer = window.setTimeout(() => ctrl.abort(), FETCH_TIMEOUT_MS);
  try {
    const results = await Promise.all(
      fallbackEdges.map(async (edge) => {
        try {
          return await fetchEdgeHealth(edge, ctrl.signal);
        } catch {
          return offlineEdge(edge);
        }
      }),
    );
    const nextEdges = [...results].sort((a, b) =>
      a.edge_id.localeCompare(b.edge_id),
    );
    edges.value = nextEdges;
    recordLatencySamples(nextEdges);
    meshState.value = nextEdges.some((edge) => edge.reachable) ? "live" : "offline";
  } catch {
    edges.value = fallbackEdges.map((edge) => offlineEdge(edge));
    meshState.value = "offline";
  } finally {
    window.clearTimeout(timer);
  }
}

onMounted(() => {
  requestAnimationFrame(() => {
    loaded.value = true;
  });
  refreshEdges();
  refetchTimer = window.setInterval(refreshEdges, REFETCH_INTERVAL_MS);
  // One-shot IP geolocation for the "you" dot on the minimap.
  resolveIpLocation();
});

onBeforeUnmount(() => {
  if (refetchTimer !== undefined) window.clearInterval(refetchTimer);
  currentAbort?.abort();
});

// Derived UI state
const healthyCount = computed(
  () =>
    edges.value.filter((e) => e.reachable && e.ready && !e.overloaded).length,
);
const totalCount = computed(() => edges.value.length);

const meshLabel = computed(() => {
  switch (meshState.value) {
    case "live":
      return "live";
    case "offline":
      return "offline";
    default:
      return "resolving";
  }
});

const meshStatusText = computed(() => {
  switch (meshState.value) {
    case "live":
      return "mesh · live";
    case "offline":
      return "mesh · offline";
    default:
      return "mesh · resolving";
  }
});

function regionStatus(e: EdgeSummary): "healthy" | "degraded" | "offline" {
  if (!e.reachable || !e.ready) return "offline";
  if (e.overloaded) return "degraded";
  return "healthy";
}

function regionLatency(e: EdgeSummary): string {
  if (e.client_rtt_ms === null || e.client_rtt_ms === undefined) {
    return "—";
  }
  if (e.client_rtt_ms > 0 && e.client_rtt_ms < 1) {
    return "<1";
  }
  return String(Math.round(e.client_rtt_ms));
}

function regionLatencyUnit(e: EdgeSummary): string {
  return e.client_rtt_ms === null || e.client_rtt_ms === undefined ? "" : "ms";
}

function regionMeshLatency(e: EdgeSummary): string | null {
  if (e.best_node_rtt_ms === null || e.best_node_rtt_ms === undefined) {
    return null;
  }
  if (e.best_node_rtt_ms === 0) {
    return "mesh <1 ms";
  }
  return `mesh ${Math.round(e.best_node_rtt_ms)} ms`;
}

// ── Minimap: show the visitor's rough location and connect it to the three
// public edges. Communicates "this is a ping from YOUR browser to these
// boxes" without an IP-geo API — just the browser's timezone, rounded to
// a handful of hubs that cover most traffic. Unknown timezones hide the
// client dot so we never pretend to know where someone is.

const EDGE_COORDS: Record<string, { lat: number; lon: number; label: string }> = {
  use1: { lat: 37.4, lon: -79.1, label: "Virginia" },
  euc1: { lat: 50.1, lon: 8.6, label: "Frankfurt" },
  ape1: { lat: 22.3, lon: 114.2, label: "Hong Kong" },
};

// Rough city → lat/lon table keyed by IANA timezone. Covers the majority
// of real-world traffic; anything else falls through to null.
const TZ_COORDS: Record<string, { lat: number; lon: number; city: string }> = {
  "America/New_York": { lat: 40.7, lon: -74.0, city: "New York" },
  "America/Chicago": { lat: 41.9, lon: -87.6, city: "Chicago" },
  "America/Denver": { lat: 39.7, lon: -105.0, city: "Denver" },
  "America/Los_Angeles": { lat: 34.0, lon: -118.2, city: "Los Angeles" },
  "America/Toronto": { lat: 43.7, lon: -79.4, city: "Toronto" },
  "America/Vancouver": { lat: 49.3, lon: -123.1, city: "Vancouver" },
  "America/Mexico_City": { lat: 19.4, lon: -99.1, city: "Mexico City" },
  "America/Sao_Paulo": { lat: -23.5, lon: -46.6, city: "São Paulo" },
  "America/Argentina/Buenos_Aires": { lat: -34.6, lon: -58.4, city: "Buenos Aires" },
  "Europe/London": { lat: 51.5, lon: -0.1, city: "London" },
  "Europe/Dublin": { lat: 53.3, lon: -6.3, city: "Dublin" },
  "Europe/Paris": { lat: 48.9, lon: 2.3, city: "Paris" },
  "Europe/Berlin": { lat: 52.5, lon: 13.4, city: "Berlin" },
  "Europe/Madrid": { lat: 40.4, lon: -3.7, city: "Madrid" },
  "Europe/Amsterdam": { lat: 52.4, lon: 4.9, city: "Amsterdam" },
  "Europe/Stockholm": { lat: 59.3, lon: 18.1, city: "Stockholm" },
  "Europe/Warsaw": { lat: 52.2, lon: 21.0, city: "Warsaw" },
  "Europe/Moscow": { lat: 55.8, lon: 37.6, city: "Moscow" },
  "Europe/Istanbul": { lat: 41.0, lon: 28.9, city: "Istanbul" },
  "Africa/Cairo": { lat: 30.0, lon: 31.2, city: "Cairo" },
  "Africa/Lagos": { lat: 6.5, lon: 3.4, city: "Lagos" },
  "Africa/Johannesburg": { lat: -26.2, lon: 28.0, city: "Johannesburg" },
  "Asia/Dubai": { lat: 25.2, lon: 55.3, city: "Dubai" },
  "Asia/Kolkata": { lat: 28.6, lon: 77.2, city: "Delhi" },
  "Asia/Bangkok": { lat: 13.8, lon: 100.5, city: "Bangkok" },
  "Asia/Singapore": { lat: 1.4, lon: 103.8, city: "Singapore" },
  "Asia/Hong_Kong": { lat: 22.3, lon: 114.2, city: "Hong Kong" },
  "Asia/Shanghai": { lat: 31.2, lon: 121.5, city: "Shanghai" },
  "Asia/Tokyo": { lat: 35.7, lon: 139.7, city: "Tokyo" },
  "Asia/Seoul": { lat: 37.6, lon: 127.0, city: "Seoul" },
  "Australia/Sydney": { lat: -33.9, lon: 151.2, city: "Sydney" },
  "Australia/Melbourne": { lat: -37.8, lon: 145.0, city: "Melbourne" },
  "Pacific/Auckland": { lat: -36.8, lon: 174.8, city: "Auckland" },
};

// IP geolocation (via ipapi.co, free + CORS-friendly). Falls back to the
// timezone heuristic if the request fails (ad-blocker, rate limit,
// offline) so the map still shows a dot in the most-likely-correct city.
const ipCoords = ref<{ lat: number; lon: number; city: string } | null>(null);

async function resolveIpLocation() {
  try {
    const res = await fetch("https://ipapi.co/json/", { cache: "force-cache" });
    if (!res.ok) return;
    const body = (await res.json()) as {
      latitude?: number;
      longitude?: number;
      city?: string;
      country_name?: string;
    };
    if (typeof body.latitude === "number" && typeof body.longitude === "number") {
      const city = body.city?.trim() || body.country_name?.trim() || "your IP";
      ipCoords.value = { lat: body.latitude, lon: body.longitude, city };
    }
  } catch {
    // Swallow: the timezone fallback handles it.
  }
}

const clientCoords = computed(() => {
  if (ipCoords.value) return ipCoords.value;
  try {
    const tz = Intl.DateTimeFormat().resolvedOptions().timeZone;
    return TZ_COORDS[tz] ?? null;
  } catch {
    return null;
  }
});

// Equirectangular projection matching the pre-rendered world-land.svg
// (viewBox 0 0 360 180, same as d3.geoEquirectangular at scale 180/π).
// That way continents, dots, lines, and labels all share a coord system.
function project(lat: number, lon: number): { x: number; y: number } {
  const x = 180 + lon;
  const y = 90 - lat;
  return { x, y };
}

type MapEdge = {
  edge_id: string;
  label: string;
  x: number;
  y: number;
  lx: number; // label position after collision-avoidance
  ly: number;
  status: "healthy" | "degraded" | "offline";
  rtt: string;
  rtt_unit: string;
};

// Raw edge positions (before label nudging).
const rawMapEdges = computed(() =>
  edges.value.map((e) => {
    const geo = EDGE_COORDS[e.edge_id] ?? { lat: 0, lon: 0, label: e.region };
    const { x, y } = project(geo.lat, geo.lon);
    return {
      edge_id: e.edge_id,
      label: geo.label,
      x,
      y,
      status: regionStatus(e),
      rtt: regionLatency(e),
      rtt_unit: regionLatencyUnit(e),
    };
  })
);

const clientPointRaw = computed(() => {
  const c = clientCoords.value;
  if (!c) return null;
  const { x, y } = project(c.lat, c.lon);
  return { ...c, x, y };
});

// Continuous physics for label orbits and line bends. Every frame the RAF
// step integrates forces on each label (repulsion from other labels, dots
// and connector lines; angular spring back to a bias direction; radial
// spring to rest radius) and each line (bend springs back to zero). The
// user can grab a label and pull it around — drag overrides physics and
// the velocity built up during drag carries through on release, so the
// label wobbles back like a pendulum. Lines are quadratic Béziers with a
// draggable midpoint that springs back the same way.
const LABEL_RADIUS = 14;      // rest distance from dot to label center
const AVOID_MARGIN = 0.10;    // keep-out buffer as fraction of object size
const LINE_CLEAR = 6;         // keep labels this far from connector lines
const EDGE_DOT_R = 5;         // edge dot halo radius (viewBox u)
const YOU_DOT_R = 6;          // viewer dot halo radius

const DAMP_PER_SEC = 38.0;    // v *= exp(-DAMP_PER_SEC·dt) — wobble decay
const ANGLE_SPRING = 6.0;     // rad/s² per rad offset from bias
const RADIUS_SPRING = 60.0;   // u/s² per unit offset from rest radius
const BEND_SPRING = 36.0;     // u/s² per unit of line bend
const TORQUE_GAIN = 75.0;     // tangential-force → angular-accel multiplier
const RADIAL_GAIN = 60.0;     // radial-force → radial-accel multiplier
const REPEL_STRENGTH = 6.0;   // multiplier on penetration-depth forces
const MAX_DT = 0.05;

// Label half-sizes in viewBox units — measured from the DOM on mount.
// Defaults roughly match current styling so the first frame looks sane
// even before the measurement runs.
const labelHalfW = ref(13);
const labelHalfH = ref(9);
const youHalfW = ref(20);
const youHalfH = ref(7);

// Each label's resting angle points OUTWARD from the map center, so
// labels visually lean toward the nearest edge of the map — like
// they're leaking off the page rather than clustering around the dots.
function outwardAngle(ax: number, ay: number): number {
  const dx = ax - 180;
  const dy = ay - 90;
  if (Math.abs(dx) < 0.01 && Math.abs(dy) < 0.01) return Math.PI / 2;
  return Math.atan2(dy, dx);
}

type LabelBody = {
  key: string;
  ax: number;         // anchor (dot) x
  ay: number;         // anchor (dot) y
  dotX: number;
  dotY: number;
  angle: number;
  radius: number;
  angleBias: number;
  angleV: number;
  radiusV: number;
};

type LineBody = {
  edgeId: string;
  bendX: number;
  bendY: number;
  bendVX: number;
  bendVY: number;
};

type Drag =
  | { kind: "label"; key: string; vbX: number; vbY: number }
  | { kind: "line"; key: string; vbX: number; vbY: number }
  | null;

const labelBodies = ref<Record<string, LabelBody>>({});
const lineBodies = ref<Record<string, LineBody>>({});
const dragState = ref<Drag>(null);
const mapInnerEl = ref<HTMLElement | null>(null);
let rafId = 0;
let lastStepTs = 0;

/// Shortest distance from (px,py) to segment [(ax,ay)-(bx,by)], plus the
/// normalized push direction (from nearest segment point toward the input
/// point). Returns null when the perpendicular foot lies near an endpoint
/// — the dot-repulsion pass handles those cases.
function perpRepel(
  px: number, py: number,
  ax: number, ay: number,
  bx: number, by: number,
): { dist: number; nx: number; ny: number } | null {
  const dx = bx - ax;
  const dy = by - ay;
  const len2 = dx * dx + dy * dy;
  if (len2 === 0) return null;
  const t = ((px - ax) * dx + (py - ay) * dy) / len2;
  if (t <= 0.05 || t >= 0.95) return null;
  const cx = ax + t * dx;
  const cy = ay + t * dy;
  const vx = px - cx;
  const vy = py - cy;
  const dist = Math.hypot(vx, vy);
  if (dist === 0) {
    const ln = Math.hypot(dx, dy);
    return { dist: 0, nx: -dy / ln, ny: dx / ln };
  }
  return { dist, nx: vx / dist, ny: vy / dist };
}

function labelPos(l: LabelBody): { x: number; y: number } {
  return { x: l.ax + l.radius * Math.cos(l.angle), y: l.ay + l.radius * Math.sin(l.angle) };
}

function halfSize(key: string): { hw: number; hh: number } {
  if (key === "__you") return { hw: youHalfW.value, hh: youHalfH.value };
  return { hw: labelHalfW.value, hh: labelHalfH.value };
}

function dotRadius(key: string): number {
  return key === "__you" ? YOU_DOT_R : EDGE_DOT_R;
}

/// Measure each label's actual rendered size and convert to viewBox units
/// so the collision math stays accurate when layout/font rendering
/// differs from our rough defaults.
function measureLabels(): void {
  const el = mapInnerEl.value;
  if (!el || el.clientWidth === 0) return;
  const scale = 360 / el.clientWidth;
  const edgeEl = el.querySelector(".sd-map-label") as HTMLElement | null;
  if (edgeEl) {
    const r = edgeEl.getBoundingClientRect();
    labelHalfW.value = (r.width / 2) * scale;
    labelHalfH.value = (r.height / 2) * scale;
  }
  const youEl = el.querySelector(".sd-map-you-label") as HTMLElement | null;
  if (youEl) {
    const r = youEl.getBoundingClientRect();
    youHalfW.value = (r.width / 2) * scale;
    youHalfH.value = (r.height / 2) * scale;
  }
}

function syncBodies(): void {
  const nextLabels: Record<string, LabelBody> = {};
  for (const e of rawMapEdges.value) {
    const prev = labelBodies.value[e.edge_id];
    const bias = outwardAngle(e.x, e.y);
    nextLabels[e.edge_id] = prev
      ? { ...prev, ax: e.x, ay: e.y, dotX: e.x, dotY: e.y, angleBias: bias }
      : {
          key: e.edge_id,
          ax: e.x, ay: e.y, dotX: e.x, dotY: e.y,
          angle: bias, angleBias: bias,
          radius: LABEL_RADIUS, angleV: 0, radiusV: 0,
        };
  }
  if (clientPointRaw.value) {
    const prev = labelBodies.value["__you"];
    const cx = clientPointRaw.value.x;
    const cy = clientPointRaw.value.y;
    const bias = outwardAngle(cx, cy);
    nextLabels["__you"] = prev
      ? { ...prev, ax: cx, ay: cy, dotX: cx, dotY: cy, angleBias: bias }
      : {
          key: "__you",
          ax: cx, ay: cy, dotX: cx, dotY: cy,
          angle: bias, angleBias: bias,
          radius: LABEL_RADIUS, angleV: 0, radiusV: 0,
        };
  }
  labelBodies.value = nextLabels;

  const nextLines: Record<string, LineBody> = {};
  for (const e of rawMapEdges.value) {
    nextLines[e.edge_id] = lineBodies.value[e.edge_id] ?? {
      edgeId: e.edge_id,
      bendX: 0, bendY: 0, bendVX: 0, bendVY: 0,
    };
  }
  lineBodies.value = nextLines;
}

watch([rawMapEdges, clientPointRaw], () => syncBodies(), { immediate: true });

function vbFromEvent(ev: PointerEvent): { x: number; y: number } {
  const el = mapInnerEl.value;
  if (!el) return { x: 0, y: 0 };
  const r = el.getBoundingClientRect();
  return {
    x: ((ev.clientX - r.left) / r.width) * 360,
    y: ((ev.clientY - r.top) / r.height) * 180,
  };
}

function startLabelDrag(ev: PointerEvent, key: string): void {
  ev.preventDefault();
  const t = ev.currentTarget as HTMLElement | null;
  t?.setPointerCapture?.(ev.pointerId);
  const pt = vbFromEvent(ev);
  dragState.value = { kind: "label", key, vbX: pt.x, vbY: pt.y };
}

function startLineDrag(ev: PointerEvent, edgeId: string): void {
  ev.preventDefault();
  const t = ev.currentTarget as SVGElement | null;
  t?.setPointerCapture?.(ev.pointerId);
  const pt = vbFromEvent(ev);
  dragState.value = { kind: "line", key: edgeId, vbX: pt.x, vbY: pt.y };
}

function onWindowPointerMove(ev: PointerEvent): void {
  if (!dragState.value) return;
  const pt = vbFromEvent(ev);
  dragState.value = { ...dragState.value, vbX: pt.x, vbY: pt.y };
}

function onWindowPointerUp(): void {
  dragState.value = null;
}

function step(ts: number): void {
  const dt = lastStepTs ? Math.min((ts - lastStepTs) / 1000, MAX_DT) : 0;
  lastStepTs = ts;
  const damp = Math.exp(-DAMP_PER_SEC * dt);
  const labels = Object.values(labelBodies.value);
  const drag = dragState.value;

  // 1. Dragged label follows cursor; derive velocity for release wobble.
  for (const l of labels) {
    if (drag?.kind === "label" && drag.key === l.key) {
      const dx = drag.vbX - l.ax;
      const dy = drag.vbY - l.ay;
      const r = Math.hypot(dx, dy);
      const a = r === 0 ? l.angle : Math.atan2(dy, dx);
      let diff = a - l.angle;
      while (diff > Math.PI) diff -= 2 * Math.PI;
      while (diff < -Math.PI) diff += 2 * Math.PI;
      if (dt > 0) {
        l.angleV = diff / dt;
        l.radiusV = (r - l.radius) / dt;
      }
      l.angle += diff;
      l.radius = Math.max(2, Math.min(60, r));
    }
  }

  // 2. Free labels: forces → tangential torque + radial push; integrate.
  // Collision model: each label is an AABB (measured pellet size) and
  // each dot is a circle (halo radius). Keep-out buffer = object size ×
  // (1 + AVOID_MARGIN) — 10% beyond actual extents. Penetration into
  // that buffer produces a linear-in-depth force scaled by
  // REPEL_STRENGTH so near-contact feels like a sharp bounce.
  for (const a of labels) {
    if (drag?.kind === "label" && drag.key === a.key) continue;
    const apos = labelPos(a);
    const aSize = halfSize(a.key);
    let fx = 0, fy = 0;

    // Label vs label (AABB with 10% margin).
    for (const b of labels) {
      if (a.key === b.key) continue;
      const bpos = labelPos(b);
      const bSize = halfSize(b.key);
      const gapX = (aSize.hw + bSize.hw) * (1 + AVOID_MARGIN);
      const gapY = (aSize.hh + bSize.hh) * (1 + AVOID_MARGIN);
      const dx = apos.x - bpos.x;
      const dy = apos.y - bpos.y;
      const penX = gapX - Math.abs(dx);
      const penY = gapY - Math.abs(dy);
      if (penX > 0 && penY > 0) {
        // Separate along the axis of least penetration (standard AABB).
        if (penX < penY) {
          fx += (dx === 0 ? 1 : Math.sign(dx)) * penX * REPEL_STRENGTH;
        } else {
          fy += (dy === 0 ? 1 : Math.sign(dy)) * penY * REPEL_STRENGTH;
        }
      }
    }

    // Label vs every dot (AABB-vs-circle with 10% margin). Own anchor
    // included — a nonzero radial repulsion pushes the label outward
    // off its dot; the radial spring pulls it back, equilibrium sits
    // just outside the threshold.
    for (const b of labels) {
      const threshold = dotRadius(b.key) * (1 + AVOID_MARGIN);
      const cx = b.dotX, cy = b.dotY;
      const clampX = Math.max(apos.x - aSize.hw, Math.min(cx, apos.x + aSize.hw));
      const clampY = Math.max(apos.y - aSize.hh, Math.min(cy, apos.y + aSize.hh));
      const dx = clampX - cx;
      const dy = clampY - cy;
      const d2 = dx * dx + dy * dy;
      if (d2 < threshold * threshold) {
        const d = Math.sqrt(d2);
        if (d < 1e-3) {
          // Dot center sits inside the box — push the label along the
          // shortest-to-escape axis.
          const ox = apos.x - cx;
          const oy = apos.y - cy;
          const nx = ox === 0 ? 0 : Math.sign(ox);
          const ny = oy === 0 ? 1 : Math.sign(oy);
          fx += nx * threshold * REPEL_STRENGTH;
          fy += ny * threshold * REPEL_STRENGTH;
        } else {
          const pen = threshold - d;
          fx += (dx / d) * pen * REPEL_STRENGTH;
          fy += (dy / d) * pen * REPEL_STRENGTH;
        }
      }
    }

    // Label vs OTHER connector lines (skip this label's own line).
    const cli = labelBodies.value["__you"];
    if (cli) {
      for (const b of labels) {
        if (b.key === "__you" || b.key === a.key) continue;
        const r = perpRepel(apos.x, apos.y, cli.dotX, cli.dotY, b.dotX, b.dotY);
        if (r && r.dist < LINE_CLEAR) {
          const push = LINE_CLEAR - r.dist;
          fx += r.nx * push * 0.6;
          fy += r.ny * push * 0.6;
        }
      }
    }

    // Decompose world-space force into radial + tangential around the
    // anchor. Tangential → torque (angle change); radial → radius change.
    const cosA = Math.cos(a.angle);
    const sinA = Math.sin(a.angle);
    const radialF = fx * cosA + fy * sinA;
    const tangentialF = -fx * sinA + fy * cosA;

    let angleDiff = a.angleBias - a.angle;
    while (angleDiff > Math.PI) angleDiff -= 2 * Math.PI;
    while (angleDiff < -Math.PI) angleDiff += 2 * Math.PI;

    const angleAccel = (tangentialF / Math.max(a.radius, 1)) * TORQUE_GAIN + angleDiff * ANGLE_SPRING;
    const radiusAccel = (LABEL_RADIUS - a.radius) * RADIUS_SPRING + radialF * RADIAL_GAIN;

    a.angleV = (a.angleV + angleAccel * dt) * damp;
    a.radiusV = (a.radiusV + radiusAccel * dt) * damp;
    a.angle += a.angleV * dt;
    a.radius += a.radiusV * dt;
    a.radius = Math.max(4, Math.min(50, a.radius));
  }

  // 3. Lines: dragged → bend follows cursor; free → bend springs back.
  const cli = labelBodies.value["__you"];
  for (const ln of Object.values(lineBodies.value)) {
    const edge = labelBodies.value[ln.edgeId];
    if (!edge || !cli) { ln.bendX = 0; ln.bendY = 0; ln.bendVX = 0; ln.bendVY = 0; continue; }
    const mx = 0.5 * (cli.dotX + edge.dotX);
    const my = 0.5 * (cli.dotY + edge.dotY);
    if (drag?.kind === "line" && drag.key === ln.edgeId) {
      const newBx = drag.vbX - mx;
      const newBy = drag.vbY - my;
      if (dt > 0) {
        ln.bendVX = (newBx - ln.bendX) / dt;
        ln.bendVY = (newBy - ln.bendY) / dt;
      }
      ln.bendX = newBx;
      ln.bendY = newBy;
    } else {
      const aX = -ln.bendX * BEND_SPRING;
      const aY = -ln.bendY * BEND_SPRING;
      ln.bendVX = (ln.bendVX + aX * dt) * damp;
      ln.bendVY = (ln.bendVY + aY * dt) * damp;
      ln.bendX += ln.bendVX * dt;
      ln.bendY += ln.bendVY * dt;
    }
  }

  rafId = requestAnimationFrame(step);
}

onMounted(() => {
  window.addEventListener("pointermove", onWindowPointerMove);
  window.addEventListener("pointerup", onWindowPointerUp);
  window.addEventListener("pointercancel", onWindowPointerUp);
  window.addEventListener("resize", measureLabels);
  rafId = requestAnimationFrame(step);
  // Wait a paint so getBoundingClientRect reflects the final pellet size.
  nextTick(() => measureLabels());
});

onBeforeUnmount(() => {
  window.removeEventListener("pointermove", onWindowPointerMove);
  window.removeEventListener("pointerup", onWindowPointerUp);
  window.removeEventListener("pointercancel", onWindowPointerUp);
  window.removeEventListener("resize", measureLabels);
  if (rafId) cancelAnimationFrame(rafId);
});

const mapEdges = computed<MapEdge[]>(() =>
  rawMapEdges.value.map((e) => {
    const body = labelBodies.value[e.edge_id];
    const p = body ? labelPos(body) : { x: e.x, y: e.y + LABEL_RADIUS };
    return { ...e, lx: p.x, ly: p.y };
  })
);

const clientPoint = computed(() => {
  if (!clientPointRaw.value) return null;
  const body = labelBodies.value["__you"];
  const p = body ? labelPos(body) : { x: clientPointRaw.value.x, y: clientPointRaw.value.y - LABEL_RADIUS };
  return { ...clientPointRaw.value, lx: p.x, ly: p.y };
});

type MapLine = { edge_id: string; status: "healthy" | "degraded" | "offline"; d: string };

const mapLines = computed<MapLine[]>(() => {
  const cli = clientPointRaw.value;
  if (!cli) return [];
  return rawMapEdges.value.map((e) => {
    const bend = lineBodies.value[e.edge_id];
    const mx = 0.5 * (cli.x + e.x);
    const my = 0.5 * (cli.y + e.y);
    const bx = bend?.bendX ?? 0;
    const by = bend?.bendY ?? 0;
    // For a quadratic Bézier P0–P1–P2, the point at t=0.5 is
    // (P0 + 2·P1 + P2) / 4. To place that midpoint at M + bend, we
    // set the control point P1 = M + 2·bend.
    const cx = mx + 2 * bx;
    const cy = my + 2 * by;
    return { edge_id: e.edge_id, status: e.status, d: `M ${cli.x} ${cli.y} Q ${cx} ${cy} ${e.x} ${e.y}` };
  });
});

// === "How it flows" — live simulation ===
//
// A continuous sim of the actual rules:
//   1. EUC1 client deposits $1000 to seed the account.
//   2. NYC and Tokyo clients fire `charge` calls at random
//      intervals; each charge routes to the local edge → a node.
//   3. The receiving node holds 5× the charge for 60s under its
//      OWN id (USE1.node-1, APE1.node-2). The hold is gossiped
//      to every peer.
//   4. Subsequent charges to the same node consume from the
//      existing hold (no new event needed).
//   5. When the hold's remaining balance runs low OR its TTL
//      drops under 5s, the node automatically rolls into a fresh
//      hold (renewal event).
//   6. When a hold's TTL hits 0, it expires; whatever balance
//      hadn't been consumed returns to the available pool.
//
// All visuals (events log, holders list, balance) come from the
// reactive state below. Timers run only while the section is in
// view (IntersectionObserver gate) so off-screen tabs are quiet.

const howflowEl = ref<HTMLElement | null>(null);
const howflowVisible = ref(false);
let howflowObserver: IntersectionObserver | null = null;

type SimHold = {
  id: number;
  holder: string;     // e.g. "USE1.node-1"
  rowKey: "a" | "b";  // which row this hold belongs to
  amount: number;     // total reserved
  remaining: number;  // unspent portion
  ttl: number;        // seconds remaining
  origin: string;     // originating client id
};

type SimEvent = {
  id: number;
  kind: "deposit" | "hold" | "renew" | "consume" | "expire";
  text: string;
  meta: string;
  age: number;        // seconds since fired (for fade-out)
};

const simCommitted = ref(0);                        // total deposits − settled spend
const simHolds = ref<SimHold[]>([]);
const simEvents = ref<SimEvent[]>([]);
const simStarted = ref(false);

const simHeld = computed(() =>
  simHolds.value.reduce((sum, h) => sum + h.amount, 0),
);
const simAvailable = computed(() =>
  Math.max(0, simCommitted.value - simHeld.value),
);

let simTickHandle = 0;
let simNextChargeAt = 0;
let simSeq = 4126;
let simEventIdCounter = 0;
let simHoldIdCounter = 0;
let simLastTickMs = 0;

function pushSimEvent(e: Omit<SimEvent, "id" | "age">): void {
  simSeq += 1;
  simEvents.value = [
    { id: ++simEventIdCounter, age: 0, ...e },
    ...simEvents.value,
  ].slice(0, 6);
}

// Pick a charge amount within a sensible micro-billing range
// (think LLM tokens / API credits): a few tenths of a cent up to
// a dime per call. With a $20 deposit that's hundreds of charges
// before depletion, so the sim runs for a long time without
// running dry.
function randCharge(): number {
  const buckets = [0.005, 0.01, 0.025, 0.05, 0.1];
  return buckets[Math.floor(Math.random() * buckets.length)];
}

// "Charge from a region": consume from the node's existing hold
// when there's room, otherwise create or renew a hold (5× the
// charge). Settles consumed portion of the prior hold on renewal.
function chargeOnRow(rowKey: "a" | "b"): void {
  const node = rowKey === "a" ? "USE1.node-1" : "APE1.node-2";
  const client = rowKey === "a" ? "client-nyc" : "client-tokyo";
  const charge = randCharge();
  const existing = simHolds.value.find((h) => h.holder === node);

  // Consume from existing hold when remaining + ttl are healthy.
  if (existing && existing.remaining >= charge && existing.ttl >= 5) {
    simHolds.value = simHolds.value.map((h) =>
      h.id === existing.id ? { ...h, remaining: h.remaining - charge } : h,
    );
    pushSimEvent({
      kind: "consume",
      text: `consume $${charge} · ${node}`,
      meta: `from ${client} · remaining $${(existing.remaining - charge).toFixed(0)} · seq ${simSeq + 1}`,
    });
    return;
  }

  // Need to create a new hold (or renew). On renewal, settle the
  // consumed portion of the prior hold against committed balance.
  let renewing = false;
  if (existing) {
    renewing = true;
    const settled = existing.amount - existing.remaining;
    simCommitted.value -= settled;
    simHolds.value = simHolds.value.filter((h) => h.id !== existing.id);
  }
  const holdAmt = charge * 5;
  if (simAvailable.value < holdAmt) {
    return; // skip this tick if we can't afford the new hold
  }
  const newHold: SimHold = {
    id: ++simHoldIdCounter,
    holder: node,
    rowKey,
    amount: holdAmt,
    remaining: holdAmt - charge,
    ttl: 60,
    origin: client,
  };
  simHolds.value = [...simHolds.value, newHold];
  pushSimEvent({
    kind: renewing ? "renew" : "hold",
    text: `${renewing ? "renew" : "hold"} $${holdAmt} · ttl 60s · ${node}`,
    meta: `from ${client} · charge $${charge} · seq ${simSeq + 1}`,
  });
}

function simTick(): void {
  const now = Date.now();
  const dt = simLastTickMs ? (now - simLastTickMs) / 1000 : 0;
  simLastTickMs = now;

  // Decrement TTLs and age out expired holds.
  let nextHolds: SimHold[] = [];
  for (const h of simHolds.value) {
    const ttl = Math.max(0, h.ttl - dt);
    if (ttl <= 0) {
      // Expired — settle consumed portion against committed
      // balance; the remaining (unspent) returns to available
      // implicitly when this hold leaves the list.
      const settled = h.amount - h.remaining;
      simCommitted.value -= settled;
      pushSimEvent({
        kind: "expire",
        text: `expire $${h.remaining.toFixed(0)} returned · ${h.holder}`,
        meta: `settled $${settled.toFixed(0)} spend · seq ${simSeq + 1}`,
      });
      continue;
    }
    nextHolds.push({ ...h, ttl });
  }
  simHolds.value = nextHolds;

  // Age events for fade-out on the older entries.
  simEvents.value = simEvents.value.map((e) => ({ ...e, age: e.age + dt }));

  // Fire next charge on its random schedule.
  if (now >= simNextChargeAt) {
    simNextChargeAt = now + 2200 + Math.random() * 2300;
    chargeOnRow(Math.random() < 0.5 ? "a" : "b");
  }

  simTickHandle = window.setTimeout(simTick, 200);
}

function startSim(): void {
  if (simStarted.value) return;
  simStarted.value = true;
  simCommitted.value = 0;
  simHolds.value = [];
  simEvents.value = [];
  simSeq = 4126;
  simEventIdCounter = 0;
  simHoldIdCounter = 0;
  simLastTickMs = 0;

  // Initial deposit from EUC1 (Paris client) seeds the account.
  window.setTimeout(() => {
    if (!simStarted.value) return;
    simCommitted.value = 20;
    pushSimEvent({
      kind: "deposit",
      text: `deposit +$20.00 · EUC1.node-2`,
      meta: `from client-paris · seq ${simSeq + 1}`,
    });
    simNextChargeAt = Date.now() + 1500;
  }, 800);

  simTickHandle = window.setTimeout(simTick, 200);
}

function stopSim(): void {
  if (simTickHandle) {
    window.clearTimeout(simTickHandle);
    simTickHandle = 0;
  }
  simStarted.value = false;
}

// Per-row hold lookup so the template can show "+ hold $X · 53s"
// directly under each region's nodes when one is active.
const simHoldByRow = computed<Record<"a" | "b", SimHold | undefined>>(() => ({
  a: simHolds.value.find((h) => h.rowKey === "a"),
  b: simHolds.value.find((h) => h.rowKey === "b"),
}));

function fmtTtl(ttl: number): string {
  return ttl >= 10 ? `${Math.round(ttl)}s` : `${ttl.toFixed(1)}s`;
}
// Magnitude-aware money formatter. Whole-dollar amounts (the
// committed balance, deposits) get the usual 2 decimals; sub-
// dollar micro-charges get 4 so $0.005 reads as $0.0050 instead
// of getting rounded to $0.01.
function fmtMoney(n: number): string {
  if (Math.abs(n) >= 1) return `$${n.toFixed(2)}`;
  return `$${n.toFixed(4)}`;
}

onMounted(() => {
  if (typeof IntersectionObserver !== "undefined" && howflowEl.value) {
    howflowObserver = new IntersectionObserver(
      (entries) => {
        const inView = entries[0]?.isIntersecting ?? false;
        howflowVisible.value = inView;
        if (inView) startSim();
        else stopSim();
      },
      { threshold: 0.2 },
    );
    howflowObserver.observe(howflowEl.value);
  }
});

onBeforeUnmount(() => {
  stopSim();
  howflowObserver?.disconnect();
});

function regionLatencySparkline(edge: EdgeSummary): string {
  const samples = latencyHistory.value[edge.edge_id] || [];
  if (samples.length === 0) return "";
  const min = Math.min(...samples);
  const max = Math.max(...samples);
  const range = Math.max(max - min, 1);
  if (samples.length === 1) {
    const y = SPARKLINE_HEIGHT / 2;
    return `0,${y.toFixed(1)} ${SPARKLINE_WIDTH},${y.toFixed(1)}`;
  }
  return samples
    .map((sample, index) => {
      const x = (index / (samples.length - 1)) * SPARKLINE_WIDTH;
      const normalized = range === 1 ? 0.5 : (sample - min) / range;
      const y = SPARKLINE_HEIGHT - normalized * SPARKLINE_HEIGHT;
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(" ");
}
</script>

<template>
  <div class="sd-home" :class="{ loaded }">
    <div class="sd-grain" aria-hidden="true"></div>

    <!-- HERO ------------------------------------------------------ -->
    <section class="sd-hero">
      <div class="sd-hero-bg" aria-hidden="true"></div>
      <div class="sd-hero-inner">
        <div class="sd-meta" data-delay="0" :data-state="meshState">
          <span class="sd-pulse">
            <span class="sd-pulse-dot"></span>
          </span>
          <span class="sd-meta-text">{{ meshStatusText }}</span>
        </div>

        <h1 class="sd-title" data-delay="1">
          Balances<br />at the edge.
        </h1>

        <div class="sd-hero-row">
          <div class="sd-hero-col" data-delay="2">
            <p class="sd-tagline">
              A distributed ledger for metered billing. Post credits,
              debits, and holds at any edge — writes replicate
              asynchronously across regions.
            </p>
            <div class="sd-actions">
              <a class="sd-btn sd-btn-primary" href="https://app.shardd.xyz">
                <div class="sd-btn-stripe"></div>
                <span>Log in</span>
                <svg class="sd-btn-arrow" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12h14"/><path d="m12 5 7 7-7 7"/></svg>
              </a>
              <a class="sd-btn sd-btn-ghost" href="/guide/quickstart">
                Read the docs
              </a>
            </div>
          </div>

          <aside
            class="sd-regions"
            data-delay="3"
            :data-state="meshState"
            aria-label="Edge mesh status"
          >
            <header class="sd-regions-head">
              <span class="sd-regions-label">edge mesh</span>
              <span class="sd-regions-count">
                <span class="sd-regions-num">{{ healthyCount }}</span>
                /
                <span class="sd-regions-num">{{ totalCount }}</span>
                <span class="sd-regions-state">· {{ meshLabel }}</span>
              </span>
            </header>

            <div class="sd-regions-body">
            <div class="sd-regions-body-left">
            <!-- Minimap: pre-rendered world-land.svg is the background;
                 dots + lines + labels overlay it in the same 360x180 coord
                 system so continents, edges and the viewer's dot agree.
                 Inner wrapper holds the aspect-ratio so labels and SVG
                 share a single coordinate system. -->
            <div class="sd-map" aria-hidden="true">
              <div class="sd-map-inner" ref="mapInnerEl">
                <img src="/world-land.svg" class="sd-map-bg" alt="" decoding="async" />
                <svg class="sd-map-svg" viewBox="0 0 360 180" preserveAspectRatio="none">
                  <template v-if="clientPoint">
                    <!-- Fat transparent hit path behind each visible line so
                         users can grab anywhere along it and pull. -->
                    <path
                      v-for="line in mapLines"
                      :key="`hit-${line.edge_id}`"
                      :d="line.d"
                      class="sd-map-link-hit"
                      @pointerdown="(ev) => startLineDrag(ev, line.edge_id)"
                    />
                    <path
                      v-for="line in mapLines"
                      :key="`line-${line.edge_id}`"
                      :d="line.d"
                      class="sd-map-link"
                      :class="[`sd-map-link-${line.status}`]"
                    />
                  </template>
                  <g v-for="e in mapEdges" :key="`edge-${e.edge_id}`">
                    <circle :cx="e.x" :cy="e.y" r="5" class="sd-map-edge-halo" :class="[`sd-map-edge-halo-${e.status}`]"/>
                    <circle :cx="e.x" :cy="e.y" r="2.2" class="sd-map-edge" :class="[`sd-map-edge-${e.status}`]"/>
                  </g>
                  <g v-if="clientPoint">
                    <circle :cx="clientPoint.x" :cy="clientPoint.y" r="6" class="sd-map-you-halo"/>
                    <circle :cx="clientPoint.x" :cy="clientPoint.y" r="2" class="sd-map-you"/>
                  </g>
                </svg>
                <div
                  v-for="e in mapEdges"
                  :key="`label-${e.edge_id}`"
                  class="sd-map-label"
                  :class="[`sd-map-label-${e.status}`, { 'sd-map-label-drag': dragState?.kind === 'label' && dragState.key === e.edge_id }]"
                  :style="{ left: `${e.lx / 3.6}%`, top: `${e.ly / 1.8}%` }"
                  @pointerdown="(ev) => startLabelDrag(ev, e.edge_id)"
                >
                  <span class="sd-map-label-code">{{ e.edge_id }}</span>
                  <span class="sd-map-label-rtt">{{ e.rtt }}<span class="sd-map-label-unit"> {{ e.rtt_unit }}</span></span>
                </div>
                <div
                  v-if="clientPoint"
                  class="sd-map-you-label"
                  :class="{ 'sd-map-label-drag': dragState?.kind === 'label' && dragState.key === '__you' }"
                  :style="{ left: `${clientPoint.lx / 3.6}%`, top: `${clientPoint.ly / 1.8}%` }"
                  @pointerdown="(ev) => startLabelDrag(ev, '__you')"
                >
                  you · {{ clientPoint.city }}
                </div>
              </div>
            </div>
            <p class="sd-map-caption">
              <span v-if="clientPoint">Ping from your browser ({{ clientPoint.city }}) to each edge · refreshes every 2.5s.</span>
              <span v-else>Ping from your browser to each public edge · refreshes every 2.5s.</span>
              <span class="sd-map-contact">Need a region added? <a href="mailto:contact@tqdm.org?subject=shardd%20region%20request">contact@tqdm.org</a></span>
            </p>
            </div>

            <div class="sd-regions-body-right">
            <ul class="sd-regions-list">
              <li
                v-for="e in edges"
                :key="e.edge_id"
                class="sd-region"
                :data-status="regionStatus(e)"
              >
                <div class="sd-region-left">
                  <span class="sd-pulse sd-pulse-sm">
                    <span class="sd-pulse-dot"></span>
                  </span>
                  <div class="sd-region-name">
                    <div class="sd-region-code">{{ e.edge_id }}</div>
                    <div class="sd-region-city">
                      {{ e.region }}
                      <span v-if="regionMeshLatency(e)">
                        · {{ regionMeshLatency(e) }}
                      </span>
                    </div>
                  </div>
                </div>
                <div class="sd-region-right">
                  <svg
                    class="sd-region-spark"
                    :viewBox="`0 0 ${SPARKLINE_WIDTH} ${SPARKLINE_HEIGHT}`"
                    aria-hidden="true"
                  >
                    <polyline
                      v-if="regionLatencySparkline(e)"
                      :points="regionLatencySparkline(e)"
                    />
                  </svg>
                  <div class="sd-region-lat">
                    <span class="sd-region-num">{{ regionLatency(e) }}</span>
                    <span class="sd-region-unit">{{ regionLatencyUnit(e) }}</span>
                  </div>
                </div>
              </li>
            </ul>
            </div>
            </div>
            <footer class="sd-regions-foot">
              <span v-if="meshState === 'live'">
                live · browser ping /gateway/health · refresh 2.5s
              </span>
              <span v-else-if="meshState === 'offline'">
                mesh unreachable · retrying
              </span>
              <span v-else>resolving · racing regional edges</span>
            </footer>
          </aside>
        </div>
      </div>
    </section>

    <!-- FEATURES -------------------------------------------------- -->
    <section class="sd-bento">
      <div class="sd-section-inner">
        <div class="sd-section-head">
          <span class="sd-section-label">// the shape</span>
          <h2 class="sd-section-title">
            Write from any region. Read from any region.
          </h2>
        </div>
        <div class="sd-bento-grid">
          <article
            v-for="(f, i) in features"
            :key="f.num"
            class="sd-card"
            :class="{
              'sd-card-lg': i === 0,
              'sd-card-wide': i === 3,
            }"
          >
            <div class="sd-card-num">{{ f.num }}</div>
            <h3 class="sd-card-title">{{ f.title }}</h3>
            <p class="sd-card-body">{{ f.body }}</p>
          </article>
        </div>
      </div>
    </section>

    <!-- HOW IT FLOWS --------------------------------------------- -->
    <!-- Left-to-right grid with four explicit columns: CLIENTS,    -->
    <!-- EDGES, NODES, GOSSIP EVENTS. Two clients (NYC, Tokyo) fire -->
    <!-- `charge` calls concurrently; their nearest edges route to  -->
    <!-- a node, the node creates a `hold` event reserving balance, -->
    <!-- gossip replicates the event everywhere. Bottom ledger      -->
    <!-- itemizes the holds per holder.                             -->
    <section
      class="sd-howflow"
      ref="howflowEl"
      :class="{ 'sd-howflow-playing': howflowVisible }"
    >
      <div class="sd-section-inner">
        <div class="sd-section-head">
          <span class="sd-section-label">// how it flows</span>
          <h2 class="sd-section-title">
            Charge from any region. Reservations replicate.
          </h2>
        </div>

        <div class="sd-howflow-stage" aria-hidden="true">
          <!-- Column header strip -->
          <div class="sd-howflow-headers">
            <div class="sd-howflow-col-head">// clients</div>
            <div class="sd-howflow-col-head">// edges</div>
            <div class="sd-howflow-col-head">// nodes</div>
            <div class="sd-howflow-col-head">// gossip events</div>
          </div>

          <!-- 4-column grid with three flow rows. -->
          <div class="sd-howflow-grid">

            <!-- ROW 1 — Flow A: NYC client → USE1 edge → USE1 node -->
            <div class="sd-howflow-cell sd-howflow-client sd-howflow-row-a">
              <div class="sd-howflow-client-head">
                <span class="sd-howflow-pulse"></span>
                <span class="sd-howflow-client-id">client · nyc</span>
              </div>
              <code class="sd-howflow-client-call">client.charge($0.005–0.10)</code>
              <div class="sd-howflow-client-ack">↳ 200 OK · 4ms</div>
            </div>
            <div class="sd-howflow-cell sd-howflow-edge sd-howflow-row-a">
              <div class="sd-howflow-edge-code">USE1</div>
              <div class="sd-howflow-edge-city">virginia</div>
            </div>
            <div class="sd-howflow-cell sd-howflow-nodes sd-howflow-row-a">
              <div class="sd-howflow-nodes-tag">USE1.nodes</div>
              <div class="sd-howflow-nodes-row">
                <span class="sd-howflow-node sd-howflow-node-chosen-a"></span>
                <span class="sd-howflow-node sd-howflow-node-peer-a" style="--n: 1"></span>
                <span class="sd-howflow-node sd-howflow-node-peer-a" style="--n: 2"></span>
              </div>
              <div
                class="sd-howflow-nodes-emit sd-howflow-nodes-emit-live"
                v-if="simHoldByRow.a"
              >
                + hold {{ fmtMoney(simHoldByRow.a.amount) }}
                · ttl {{ fmtTtl(simHoldByRow.a.ttl) }}
                · left {{ fmtMoney(simHoldByRow.a.remaining) }}
              </div>
            </div>

            <!-- ROW 2 — EUC1: deposit from Paris, then receives gossip -->
            <div class="sd-howflow-cell sd-howflow-client sd-howflow-row-eu">
              <div class="sd-howflow-client-head">
                <span class="sd-howflow-pulse"></span>
                <span class="sd-howflow-client-id">client · paris</span>
              </div>
              <code class="sd-howflow-client-call">client.deposit($20.00)</code>
              <div class="sd-howflow-client-ack">↳ 200 OK · seeded</div>
            </div>
            <div class="sd-howflow-cell sd-howflow-edge">
              <div class="sd-howflow-edge-code">EUC1</div>
              <div class="sd-howflow-edge-city">frankfurt</div>
            </div>
            <div class="sd-howflow-cell sd-howflow-nodes">
              <div class="sd-howflow-nodes-tag">EUC1.nodes</div>
              <div class="sd-howflow-nodes-row">
                <span class="sd-howflow-node sd-howflow-node-gossip" style="--n: 0"></span>
                <span class="sd-howflow-node sd-howflow-node-eu-chosen" style="--n: 1"></span>
                <span class="sd-howflow-node sd-howflow-node-gossip" style="--n: 2"></span>
              </div>
              <div class="sd-howflow-nodes-recv">
                ↳ converged · all peers know every hold
              </div>
            </div>

            <!-- ROW 3 — Flow B: Tokyo client → APE1 edge → APE1 node -->
            <div class="sd-howflow-cell sd-howflow-client sd-howflow-row-b">
              <div class="sd-howflow-client-head">
                <span class="sd-howflow-pulse"></span>
                <span class="sd-howflow-client-id">client · tokyo</span>
              </div>
              <code class="sd-howflow-client-call">client.charge($0.005–0.10)</code>
              <div class="sd-howflow-client-ack">↳ 200 OK · 6ms</div>
            </div>
            <div class="sd-howflow-cell sd-howflow-edge sd-howflow-row-b">
              <div class="sd-howflow-edge-code">APE1</div>
              <div class="sd-howflow-edge-city">hong kong</div>
            </div>
            <div class="sd-howflow-cell sd-howflow-nodes sd-howflow-row-b">
              <div class="sd-howflow-nodes-tag">APE1.nodes</div>
              <div class="sd-howflow-nodes-row">
                <span class="sd-howflow-node sd-howflow-node-peer-b" style="--n: 0"></span>
                <span class="sd-howflow-node sd-howflow-node-chosen-b"></span>
                <span class="sd-howflow-node sd-howflow-node-peer-b" style="--n: 2"></span>
              </div>
              <div
                class="sd-howflow-nodes-emit sd-howflow-nodes-emit-live"
                v-if="simHoldByRow.b"
              >
                + hold {{ fmtMoney(simHoldByRow.b.amount) }}
                · ttl {{ fmtTtl(simHoldByRow.b.ttl) }}
                · left {{ fmtMoney(simHoldByRow.b.remaining) }}
              </div>
            </div>

            <!-- Gossip events panel — populated live from the sim. -->
            <div class="sd-howflow-events">
              <div class="sd-howflow-events-header">
                shardd/events/v1
              </div>
              <TransitionGroup name="sd-howflow-event" tag="ul" class="sd-howflow-events-list">
                <li
                  v-for="ev in simEvents"
                  :key="ev.id"
                  class="sd-howflow-event"
                  :class="`sd-howflow-event-${ev.kind}`"
                >
                  <span class="sd-howflow-event-prefix">{{
                    ev.kind === "renew" ? "↻"
                    : ev.kind === "expire" ? "✕"
                    : ev.kind === "consume" ? "−"
                    : ev.kind === "deposit" ? "+"
                    : "▸"
                  }}</span>
                  <div class="sd-howflow-event-body">
                    <div class="sd-howflow-event-payload">{{ ev.text }}</div>
                    <div class="sd-howflow-event-source">{{ ev.meta }}</div>
                  </div>
                </li>
                <li
                  v-if="simEvents.length === 0"
                  key="empty"
                  class="sd-howflow-event sd-howflow-event-idle"
                >
                  <span class="sd-howflow-event-prefix">·</span>
                  <div class="sd-howflow-event-body">
                    <div class="sd-howflow-event-payload">awaiting events…</div>
                    <div class="sd-howflow-event-source">{{ simStarted ? "deposit incoming" : "section out of view" }}</div>
                  </div>
                </li>
              </TransitionGroup>
              <div class="sd-howflow-events-foot">
                ↳ every node converges · ttl 60s · auto-rolls when low
              </div>
            </div>
          </div>

          <!-- Animated request packets travel left → right along
               their row, from client cell to node cell. -->
          <div class="sd-howflow-packet sd-howflow-packet-a">
            client.charge($30)
          </div>
          <div class="sd-howflow-packet sd-howflow-packet-b">
            client.charge($20)
          </div>
        </div>

        <!-- Live ledger — driven entirely from the sim state. -->
        <div class="sd-howflow-ledger">
          <div class="sd-howflow-ledger-cell">
            <div class="sd-howflow-ledger-label">committed balance</div>
            <div class="sd-howflow-ledger-val">{{ fmtMoney(simCommitted) }}</div>
          </div>
          <div class="sd-howflow-ledger-divider"></div>
          <div class="sd-howflow-ledger-cell sd-howflow-ledger-holds">
            <div class="sd-howflow-ledger-label">
              active holds
              <span class="sd-howflow-ledger-sublabel">
                · {{ simHolds.length }}
              </span>
            </div>
            <TransitionGroup name="sd-howflow-holder" tag="ul" class="sd-howflow-holders">
              <li
                v-for="h in simHolds"
                :key="h.id"
                class="sd-howflow-holder"
                :class="`sd-howflow-holder-${h.rowKey}`"
              >
                <span class="sd-howflow-holder-id">{{ h.holder }}</span>
                <span class="sd-howflow-holder-meta">
                  <span class="sd-howflow-holder-amt">−{{ fmtMoney(h.remaining) }}</span>
                  <span class="sd-howflow-holder-of">/ {{ fmtMoney(h.amount) }}</span>
                  <span class="sd-howflow-holder-ttl" :class="{ 'sd-howflow-holder-ttl-low': h.ttl < 8 }">
                    {{ fmtTtl(h.ttl) }}
                  </span>
                </span>
              </li>
              <li
                v-if="simHolds.length === 0"
                key="none"
                class="sd-howflow-holder sd-howflow-holder-empty"
              >
                <span class="sd-howflow-holder-id">no active holds</span>
              </li>
            </TransitionGroup>
          </div>
          <div class="sd-howflow-ledger-divider"></div>
          <div class="sd-howflow-ledger-cell">
            <div class="sd-howflow-ledger-label">available</div>
            <div class="sd-howflow-ledger-val">{{ fmtMoney(simAvailable) }}</div>
          </div>
        </div>

        <p class="sd-howflow-caption">
          Two clients fire <code>charge</code> calls concurrently to
          their nearest edge. The receiving node holds 5× the charge
          for 60s under its own id and gossips the
          <code>hold</code> event so every peer converges on the
          same reservation. When a hold's remaining balance and TTL
          both run low, the holding node automatically rolls into a
          fresh hold — no client retry, no coordination.
        </p>
      </div>
    </section>

    <!-- CTA ------------------------------------------------------- -->
    <section class="sd-cta">
      <div class="sd-section-inner">
        <div class="sd-cta-inner">
          <div class="sd-cta-text">
            <span class="sd-section-label">// start here</span>
            <h2 class="sd-cta-title">POST /events.</h2>
            <p class="sd-cta-sub">
              Credit or debit any account at any edge. Read balances
              from anywhere. The mesh handles the rest.
            </p>
          </div>
          <div class="sd-cta-actions">
            <a class="sd-btn sd-btn-primary" href="/guide/quickstart">
              <div class="sd-btn-stripe"></div>
              <span>Quickstart</span>
              <svg class="sd-btn-arrow" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12h14"/><path d="m12 5 7 7-7 7"/></svg>
            </a>
            <a
              class="sd-btn sd-btn-ghost"
              href="https://github.com/shardd-xyz/shardd"
            >
              Read the source
            </a>
          </div>
        </div>
      </div>
    </section>
  </div>
</template>
