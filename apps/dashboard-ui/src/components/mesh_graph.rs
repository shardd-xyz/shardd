use crate::types::{EdgeHealth, EdgeInfo};
use dioxus::prelude::*;

#[derive(Clone)]
struct EdgeState {
    label: String,
    node_label: String,
    region: String,
    health: Option<EdgeHealth>,
    client_rtt_ms: Option<f64>,
    node_rtts: Vec<(String, u64)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    Pan {
        cx0: f64,
        cy0: f64,
        vx0: f64,
        vy0: f64,
    },
    Node {
        idx: usize,
        ox: f64,
        oy: f64,
    }, // unified: 0..n = edge, n..2n = mesh
}

fn to_svg(client_x: f64, client_y: f64, vb: (f64, f64, f64, f64)) -> (f64, f64) {
    let win = match web_sys::window() {
        Some(w) => w,
        None => return (client_x, client_y),
    };
    let doc = match win.document() {
        Some(d) => d,
        None => return (client_x, client_y),
    };
    let el = match doc.get_element_by_id("mesh-topo-svg") {
        Some(e) => e,
        None => return (client_x, client_y),
    };
    let r = el.get_bounding_client_rect();
    if r.width() == 0.0 || r.height() == 0.0 {
        return (client_x, client_y);
    }
    (
        vb.0 + ((client_x - r.left()) / r.width()) * vb.2,
        vb.1 + ((client_y - r.top()) / r.height()) * vb.3,
    )
}

const BASE_W: f64 = 700.0;
const BASE_H: f64 = 500.0;
const CX: f64 = 350.0;
const CY: f64 = 240.0;
const OUTER_R: f64 = 180.0;
const INNER_R: f64 = 80.0;

fn pill_w(label: &str) -> f64 {
    (label.len() as f64 * 5.8 + 20.0).max(54.0)
}

// --------------- force simulation ---------------

#[derive(Clone)]
struct Sim {
    px: Vec<f64>,
    py: Vec<f64>,
    vx: Vec<f64>,
    vy: Vec<f64>,
    alpha: f64,
}

impl Sim {
    fn new(n: usize) -> Self {
        let total = 2 * n;
        let mut px = Vec::with_capacity(total);
        let mut py = Vec::with_capacity(total);
        for i in 0..n {
            let a =
                -std::f64::consts::FRAC_PI_2 + (i as f64) * 2.0 * std::f64::consts::PI / (n as f64);
            px.push(CX + OUTER_R * a.cos());
            py.push(CY + OUTER_R * a.sin());
        }
        for i in 0..n {
            let a =
                -std::f64::consts::FRAC_PI_2 + (i as f64) * 2.0 * std::f64::consts::PI / (n as f64);
            px.push(CX + INNER_R * a.cos());
            py.push(CY + INNER_R * a.sin());
        }
        Self {
            vx: vec![0.0; total],
            vy: vec![0.0; total],
            px,
            py,
            alpha: 1.0,
        }
    }

    /// `widths[i]` = pill width of node i, `heights[i]` = pill height.
    fn tick(&mut self, n: usize, pinned: Option<usize>, widths: &[f64], heights: &[f64]) {
        let total = 2 * n;
        if total == 0 {
            return;
        }
        let k_repel = 6000.0;
        let k_local = 0.06;
        let k_cross = 0.008;
        let rest_local = 100.0;
        let rest_cross = 200.0;
        let k_center = 0.008;
        let damping = 0.82;
        let margin = 12.0; // extra gap between node edges

        let mut fx = vec![0.0_f64; total];
        let mut fy = vec![0.0_f64; total];

        // repulsion — all pairs
        for i in 0..total {
            for j in (i + 1)..total {
                let dx = self.px[i] - self.px[j];
                let dy = self.py[i] - self.py[j];
                let d2 = (dx * dx + dy * dy).max(1.0);
                let d = d2.sqrt();
                let f = k_repel / d2;
                let ux = dx / d;
                let uy = dy / d;
                fx[i] += f * ux;
                fy[i] += f * uy;
                fx[j] -= f * ux;
                fy[j] -= f * uy;
            }
        }

        // springs — edge[i] ↔ mesh[j]
        for i in 0..n {
            for j in 0..n {
                let mi = n + j;
                let local = i == j;
                let (k, rest) = if local {
                    (k_local, rest_local)
                } else {
                    (k_cross, rest_cross)
                };
                let dx = self.px[mi] - self.px[i];
                let dy = self.py[mi] - self.py[i];
                let d = (dx * dx + dy * dy).sqrt().max(1.0);
                let f = k * (d - rest);
                let ux = dx / d;
                let uy = dy / d;
                fx[i] += f * ux;
                fy[i] += f * uy;
                fx[mi] -= f * ux;
                fy[mi] -= f * uy;
            }
        }

        // center gravity
        for i in 0..total {
            fx[i] += k_center * (CX - self.px[i]);
            fy[i] += k_center * (CY - self.py[i]);
        }

        // integrate
        for i in 0..total {
            if Some(i) == pinned {
                continue;
            }
            self.vx[i] = (self.vx[i] + fx[i]) * damping;
            self.vy[i] = (self.vy[i] + fy[i]) * damping;
            let spd = (self.vx[i] * self.vx[i] + self.vy[i] * self.vy[i]).sqrt();
            if spd > 12.0 {
                self.vx[i] *= 12.0 / spd;
                self.vy[i] *= 12.0 / spd;
            }
            self.px[i] += self.vx[i] * self.alpha;
            self.py[i] += self.vy[i] * self.alpha;
        }

        // collision resolution — axis-aligned bounding-box overlap push
        // run multiple passes for stability
        for _ in 0..3 {
            for i in 0..total {
                for j in (i + 1)..total {
                    if Some(i) == pinned && Some(j) == pinned {
                        continue;
                    }
                    let hw_i = widths[i] / 2.0 + margin / 2.0;
                    let hh_i = heights[i] / 2.0 + margin / 2.0;
                    let hw_j = widths[j] / 2.0 + margin / 2.0;
                    let hh_j = heights[j] / 2.0 + margin / 2.0;

                    let dx = self.px[i] - self.px[j];
                    let dy = self.py[i] - self.py[j];
                    let overlap_x = (hw_i + hw_j) - dx.abs();
                    let overlap_y = (hh_i + hh_j) - dy.abs();

                    if overlap_x > 0.0 && overlap_y > 0.0 {
                        // push apart along the axis with less overlap
                        if overlap_x < overlap_y {
                            let push = overlap_x / 2.0 * dx.signum();
                            if Some(i) != pinned {
                                self.px[i] += push;
                            }
                            if Some(j) != pinned {
                                self.px[j] -= push;
                            }
                        } else {
                            let push = overlap_y / 2.0 * dy.signum();
                            if Some(i) != pinned {
                                self.py[i] += push;
                            }
                            if Some(j) != pinned {
                                self.py[j] -= push;
                            }
                        }
                        // also kill relative velocity to prevent jitter
                        if Some(i) != pinned {
                            self.vx[i] *= 0.5;
                            self.vy[i] *= 0.5;
                        }
                        if Some(j) != pinned {
                            self.vx[j] *= 0.5;
                            self.vy[j] *= 0.5;
                        }
                    }
                }
            }
        }

        self.alpha *= 0.99;
        if self.alpha < 0.001 {
            self.alpha = 0.001;
        }
    }

    fn ep(&self, i: usize) -> (f64, f64) {
        (self.px[i], self.py[i])
    }
    fn mp(&self, n: usize, i: usize) -> (f64, f64) {
        (self.px[n + i], self.py[n + i])
    }
}

// --------------- component ---------------

#[component]
pub fn MeshTopologyGraph(edges: Vec<EdgeInfo>) -> Element {
    let n = edges.len();

    let mut states: Signal<Vec<EdgeState>> = use_signal(|| {
        edges
            .iter()
            .map(|e| EdgeState {
                label: e.label.clone().unwrap_or_else(|| e.edge_id.clone()),
                node_label: e
                    .node_label
                    .clone()
                    .unwrap_or_else(|| e.edge_id.to_uppercase()),
                region: e.region.clone(),
                health: None,
                client_rtt_ms: None,
                node_rtts: vec![],
            })
            .collect()
    });

    // health polling
    use_effect(move || {
        let edge_info: Vec<(String, String, String, String)> = edges
            .iter()
            .map(|e| {
                (
                    e.label.clone().unwrap_or_else(|| e.edge_id.clone()),
                    e.node_label
                        .clone()
                        .unwrap_or_else(|| e.edge_id.to_uppercase()),
                    e.region.clone(),
                    e.base_url.clone(),
                )
            })
            .collect();
        spawn(async move {
            loop {
                let mut updated = Vec::new();
                for (label, node_label, region, base_url) in &edge_info {
                    let url = format!("{}/gateway/health", base_url.trim_end_matches('/'));
                    let start = js_sys::Date::now();
                    let result = gloo_net::http::Request::get(&url).send().await;
                    let rtt = js_sys::Date::now() - start;
                    let mut node_rtts = vec![];
                    let health = match result {
                        Ok(res) if res.ok() => {
                            let metrics_url = format!("{}/metrics", base_url.trim_end_matches('/'));
                            if let Ok(mres) =
                                gloo_net::http::Request::get(&metrics_url).send().await
                                && let Ok(text) = mres.text().await
                            {
                                for line in text.lines() {
                                    if line.starts_with("shardd_node_ping_rtt_ms{")
                                        && let (Some(node), Some(val)) = (
                                            line.split("node=\"")
                                                .nth(1)
                                                .and_then(|s| s.split('"').next()),
                                            line.split("} ").nth(1),
                                        )
                                        && let Ok(v) = val.parse::<u64>()
                                    {
                                        node_rtts.push((node.to_string(), v));
                                    }
                                }
                            }
                            res.json::<EdgeHealth>().await.ok()
                        }
                        _ => None,
                    };
                    let has_health = health.is_some();
                    updated.push(EdgeState {
                        label: label.clone(),
                        node_label: node_label.clone(),
                        region: region.clone(),
                        health,
                        client_rtt_ms: if has_health { Some(rtt) } else { None },
                        node_rtts,
                    });
                }
                states.set(updated);
                gloo_timers::future::TimeoutFuture::new(5000).await;
            }
        });
    });

    let s = states.read();
    if n == 0 {
        return rsx! { div { class: "text-base-500 font-mono text-[14px]", "No edges configured." } };
    }

    let mut sim: Signal<Sim> = use_signal(|| Sim::new(n));
    let mut vb = use_signal(|| (0.0_f64, 0.0_f64, BASE_W, BASE_H));
    let mut drag: Signal<Drag> = use_signal(|| Drag::None);

    // Pre-compute node bounding-box sizes for collision detection.
    // Order: [edge_0..edge_n, mesh_0..mesh_n]
    let node_widths: Vec<f64> = use_signal(|| {
        let st = states.read();
        let mut w = Vec::with_capacity(2 * n);
        for st in st.iter() {
            w.push(pill_w(&st.label).max(pill_w(&st.region)));
        }
        for st in st.iter() {
            w.push(pill_w(&st.node_label));
        }
        w
    })
    .read()
    .clone();
    let node_heights: Vec<f64> = use_signal(|| {
        let mut h = Vec::with_capacity(2 * n);
        h.extend((0..n).map(|_| 38.0)); // edge pill height
        h.extend((0..n).map(|_| 26.0)); // mesh pill height
        h
    })
    .read()
    .clone();

    // simulation loop
    use_effect(move || {
        let widths = node_widths.clone();
        let heights = node_heights.clone();
        spawn(async move {
            loop {
                {
                    let pinned = match *drag.read() {
                        Drag::Node { idx, .. } => Some(idx),
                        _ => None,
                    };
                    let mut sm = sim.write();
                    sm.tick(n, pinned, &widths, &heights);
                }
                let alpha = sim.read().alpha;
                let ms = if alpha < 0.01 { 100 } else { 16 };
                gloo_timers::future::TimeoutFuture::new(ms).await;
            }
        });
    });

    // snapshot for rendering
    let sm = sim.read();
    let vb_val = *vb.read();
    let is_dragging = *drag.read() != Drag::None;
    let vb_str = format!("{} {} {} {}", vb_val.0, vb_val.1, vb_val.2, vb_val.3);
    let cursor = if is_dragging { "grabbing" } else { "grab" };
    let mesh_labels: Vec<String> = s.iter().map(|st| st.node_label.clone()).collect();

    // pre-compute positions
    let ep_vals: Vec<(f64, f64)> = (0..n).map(|i| sm.ep(i)).collect();
    let mp_vals: Vec<(f64, f64)> = (0..n).map(|i| sm.mp(n, i)).collect();
    drop(sm);

    rsx! {
        section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-4",
            div { class: "flex items-center justify-between",
                h2 { class: "text-[16px] font-mono font-normal text-fg", "Mesh topology" }
                div { class: "flex items-center gap-1",
                    button {
                        class: "w-7 h-7 flex items-center justify-center text-[14px] font-mono text-base-500 hover:text-fg rounded border border-base-800 hover:border-base-700 transition",
                        onclick: move |_| {
                            let v = *vb.read();
                            let nw = (v.2 * 0.8).max(200.0);
                            let nh = (v.3 * 0.8).max(140.0);
                            vb.set((v.0 + (v.2 - nw) / 2.0, v.1 + (v.3 - nh) / 2.0, nw, nh));
                        },
                        "+"
                    }
                    button {
                        class: "w-7 h-7 flex items-center justify-center text-[14px] font-mono text-base-500 hover:text-fg rounded border border-base-800 hover:border-base-700 transition",
                        onclick: move |_| {
                            let v = *vb.read();
                            let nw = (v.2 * 1.25).min(2800.0);
                            let nh = (v.3 * 1.25).min(2000.0);
                            vb.set((v.0 + (v.2 - nw) / 2.0, v.1 + (v.3 - nh) / 2.0, nw, nh));
                        },
                        "\u{2212}"
                    }
                    button {
                        class: "h-7 px-2 text-[11px] font-mono text-base-500 hover:text-fg rounded border border-base-800 hover:border-base-700 transition",
                        onclick: move |_| {
                            sim.set(Sim::new(n));
                            vb.set((0.0, 0.0, BASE_W, BASE_H));
                        },
                        "Reset"
                    }
                }
            }

            svg {
                id: "mesh-topo-svg",
                view_box: "{vb_str}",
                class: "w-full max-w-[700px]",
                style: "background: var(--color-base-1000); border-radius: 8px; cursor: {cursor}; user-select: none; -webkit-user-select: none;",

                onmousedown: move |e: MouseEvent| {
                    let cc = e.client_coordinates();
                    let v = *vb.read();
                    drag.set(Drag::Pan { cx0: cc.x, cy0: cc.y, vx0: v.0, vy0: v.1 });
                },

                onmousemove: move |e: MouseEvent| {
                    let d = *drag.read();
                    match d {
                        Drag::None => {}
                        Drag::Pan { cx0, cy0, vx0, vy0 } => {
                            let cc = e.client_coordinates();
                            if let Some(el) = web_sys::window()
                                .and_then(|w| w.document())
                                .and_then(|d| d.get_element_by_id("mesh-topo-svg"))
                            {
                                let r = el.get_bounding_client_rect();
                                let v = *vb.read();
                                let dx = ((cc.x - cx0) / r.width()) * v.2;
                                let dy = ((cc.y - cy0) / r.height()) * v.3;
                                vb.set((vx0 - dx, vy0 - dy, v.2, v.3));
                            }
                        }
                        Drag::Node { idx, ox, oy } => {
                            let cc = e.client_coordinates();
                            let v = *vb.read();
                            let (sx, sy) = to_svg(cc.x, cc.y, v);
                            let mut sm = sim.write();
                            sm.px[idx] = sx - ox;
                            sm.py[idx] = sy - oy;
                            sm.vx[idx] = 0.0;
                            sm.vy[idx] = 0.0;
                            sm.alpha = sm.alpha.max(0.3);
                        }
                    }
                },

                onmouseup: move |_| { drag.set(Drag::None); },
                onmouseleave: move |_| { drag.set(Drag::None); },

                onwheel: move |e: WheelEvent| {
                    e.prevent_default();
                    let delta = e.data().delta().strip_units();
                    let dy = delta.y;
                    let factor = if dy > 0.0 { 1.1 } else { 1.0 / 1.1 };
                    let v = *vb.read();
                    let nw = (v.2 * factor).clamp(200.0, 2800.0);
                    let nh = (v.3 * factor).clamp(140.0, 2000.0);
                    vb.set((v.0 + (v.2 - nw) / 2.0, v.1 + (v.3 - nh) / 2.0, nw, nh));
                },

                // hit target
                rect { x: "-5000", y: "-5000", width: "10000", height: "10000", fill: "transparent" }

                // ---------- links ----------
                for (i, st) in s.iter().enumerate() {
                    for (j, ml) in mesh_labels.iter().enumerate() {
                        {
                            let (x1, y1) = ep_vals[i];
                            let (x2, y2) = mp_vals[j];
                            let rtt = st.node_rtts.iter().find(|(nd, _)| nd == ml).map(|(_, v)| *v);
                            let local = i == j;
                            let sc = if local { "#ef6f2e" } else { "#4d4947" };
                            let sw = if local { "2" } else { "1" };
                            let op = if rtt.is_some() { if local { "0.6" } else { "0.18" } } else { "0.06" };
                            let mx = (x1 + x2) / 2.0;
                            let my = (y1 + y2) / 2.0 - 3.0;
                            let rtt_text = rtt.map(|v| if v < 1 { "<1".to_string() } else { format!("{v}") }).unwrap_or_default();
                            let show = !rtt_text.is_empty() && (local || rtt.unwrap_or(0) > 5);
                            rsx! {
                                line {
                                    x1: "{x1}", y1: "{y1}", x2: "{x2}", y2: "{y2}",
                                    stroke: "{sc}", stroke_width: "{sw}", opacity: "{op}",
                                    style: "pointer-events: none;",
                                }
                                if show {
                                    text {
                                        x: "{mx}", y: "{my}",
                                        text_anchor: "middle", font_size: "8", fill: "#8a8380", font_family: "monospace",
                                        style: "pointer-events: none;",
                                        "{rtt_text}ms"
                                    }
                                }
                            }
                        }
                    }
                }

                // ---------- mesh nodes (angular pills) ----------
                for (i, st) in s.iter().enumerate() {
                    {
                        let (x, y) = mp_vals[i];
                        let node_idx = n + i;
                        let pw = pill_w(&st.node_label);
                        let ph = 26.0_f64;
                        let px_l = x - pw / 2.0;
                        let py_t = y - ph / 2.0;
                        let ty = y + 3.5;
                        let fill = if st.health.as_ref().map(|h| h.ready).unwrap_or(false) { "#0F8B8D" } else { "#5c5855" };
                        rsx! {
                            g {
                                cursor: "pointer",
                                onmousedown: move |e: MouseEvent| {
                                    e.stop_propagation();
                                    let cc = e.client_coordinates();
                                    let v = *vb.read();
                                    let (sx, sy) = to_svg(cc.x, cc.y, v);
                                    drag.set(Drag::Node { idx: node_idx, ox: sx - x, oy: sy - y });
                                    sim.write().alpha = 0.3;
                                },
                                rect {
                                    x: "{px_l}", y: "{py_t}", width: "{pw}", height: "{ph}",
                                    rx: "4", fill: "#1f1d1c", stroke: "{fill}", stroke_width: "2",
                                }
                                text {
                                    x: "{x}", y: "{ty}",
                                    text_anchor: "middle", font_size: "9", fill: "#d6d3d2", font_family: "monospace",
                                    style: "pointer-events: none;",
                                    "{st.node_label}"
                                }
                            }
                        }
                    }
                }

                // ---------- edge nodes (rounded pills) ----------
                for (i, st) in s.iter().enumerate() {
                    {
                        let (x, y) = ep_vals[i];
                        let pw = pill_w(&st.label).max(pill_w(&st.region));
                        let ph = 38.0_f64;
                        let px_l = x - pw / 2.0;
                        let py_t = y - ph / 2.0;
                        let ty1 = y - 3.0;
                        let ty2 = y + 9.0;
                        let fill = if st.health.as_ref().map(|h| h.ready).unwrap_or(false) { "#ef6f2e" } else { "#5c5855" };
                        let rtt_text = st.client_rtt_ms.map(|ms| format!("{}ms", ms.round() as u64)).unwrap_or("\u{2014}".to_string());
                        let nodes_text = st.health.as_ref().map(|h| format!("{}/{}", h.healthy_nodes, h.discovered_nodes)).unwrap_or_default();
                        let stats = format!("{rtt_text} \u{b7} {nodes_text}");
                        let stats_y = y + ph / 2.0 + 14.0;
                        rsx! {
                            g {
                                cursor: "pointer",
                                onmousedown: move |e: MouseEvent| {
                                    e.stop_propagation();
                                    let cc = e.client_coordinates();
                                    let v = *vb.read();
                                    let (sx, sy) = to_svg(cc.x, cc.y, v);
                                    drag.set(Drag::Node { idx: i, ox: sx - x, oy: sy - y });
                                    sim.write().alpha = 0.3;
                                },
                                rect {
                                    x: "{px_l}", y: "{py_t}", width: "{pw}", height: "{ph}",
                                    rx: "14", fill: "#1f1d1c", stroke: "{fill}", stroke_width: "2",
                                }
                                text {
                                    x: "{x}", y: "{ty1}",
                                    text_anchor: "middle", font_size: "9", fill: "#fafafa", font_family: "monospace",
                                    style: "pointer-events: none;",
                                    "{st.label}"
                                }
                                text {
                                    x: "{x}", y: "{ty2}",
                                    text_anchor: "middle", font_size: "8", fill: "#8a8380", font_family: "monospace",
                                    style: "pointer-events: none;",
                                    "{st.region}"
                                }
                                text {
                                    x: "{x}", y: "{stats_y}",
                                    text_anchor: "middle", font_size: "8", fill: "#a49d9a", font_family: "monospace",
                                    style: "pointer-events: none;",
                                    "{stats}"
                                }
                            }
                        }
                    }
                }

                // legend
                rect { x: "20", y: "470", width: "24", height: "14", rx: "7", fill: "none", stroke: "#ef6f2e", stroke_width: "1.5", style: "pointer-events: none;" }
                text { x: "50", y: "481", font_size: "9", fill: "#8a8380", font_family: "monospace", style: "pointer-events: none;", "edge" }
                rect { x: "88", y: "470", width: "24", height: "14", rx: "2", fill: "none", stroke: "#0F8B8D", stroke_width: "1.5", style: "pointer-events: none;" }
                text { x: "118", y: "481", font_size: "9", fill: "#8a8380", font_family: "monospace", style: "pointer-events: none;", "mesh" }
                line { x1: "156", y1: "477", x2: "181", y2: "477", stroke: "#ef6f2e", stroke_width: "2", opacity: "0.6", style: "pointer-events: none;" }
                text { x: "188", y: "481", font_size: "9", fill: "#8a8380", font_family: "monospace", style: "pointer-events: none;", "local" }
                line { x1: "226", y1: "477", x2: "251", y2: "477", stroke: "#4d4947", stroke_width: "1", opacity: "0.3", style: "pointer-events: none;" }
                text { x: "258", y: "481", font_size: "9", fill: "#8a8380", font_family: "monospace", style: "pointer-events: none;", "cross-region" }
            }

            // RTT matrix
            div { class: "overflow-auto",
                table { class: "w-full border-collapse font-mono text-[12px]",
                    thead {
                        tr {
                            th { class: "px-3 py-2 text-left text-base-500 uppercase tracking-[-0.015rem]", "Edge \u{2192} Node" }
                            for ml in &mesh_labels {
                                th { class: "px-3 py-2 text-right text-base-500 uppercase tracking-[-0.015rem]", "{ml}" }
                            }
                        }
                    }
                    tbody {
                        for st in s.iter() {
                            tr { class: "border-t border-base-800",
                                td { class: "px-3 py-2 text-fg", "{st.label}" }
                                for ml in &mesh_labels {
                                    {
                                        let rtt = st.node_rtts.iter().find(|(nd, _)| nd == ml).map(|(_, v)| *v);
                                        let color = match rtt {
                                            Some(0) | Some(1) => "text-accent-100",
                                            Some(v) if v < 100 => "text-fg",
                                            Some(_) => "text-accent-200",
                                            None => "text-base-600",
                                        };
                                        let text = rtt.map(|v| if v < 1 { "<1".to_string() } else { v.to_string() }).unwrap_or("\u{2014}".to_string());
                                        rsx! { td { class: "px-3 py-2 text-right {color}", "{text} ms" } }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
