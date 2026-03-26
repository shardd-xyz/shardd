use dioxus::prelude::*;

#[derive(Debug, Clone, PartialEq)]
pub struct SyncTrace {
    pub total_nodes: usize,
    pub target_event_count: usize,
    /// (elapsed_ms, nodes_synced)
    pub data_points: Vec<(f64, usize)>,
    pub complete: bool,
}

#[component]
pub fn SyncChart(trace: SyncTrace) -> Element {
    if trace.data_points.is_empty() {
        return rsx! {};
    }

    let total = trace.total_nodes;
    let max_time = trace
        .data_points
        .last()
        .map(|(t, _)| *t)
        .unwrap_or(1.0)
        .max(100.0);

    // SVG dimensions
    let w = 700.0_f64;
    let h = 250.0_f64;
    let pad_l = 50.0_f64;
    let pad_r = 20.0_f64;
    let pad_t = 20.0_f64;
    let pad_b = 40.0_f64;
    let plot_w = w - pad_l - pad_r;
    let plot_h = h - pad_t - pad_b;

    // Scale functions
    let x = |t: f64| -> f64 { pad_l + (t / max_time) * plot_w };
    let y = |n: usize| -> f64 { pad_t + plot_h - (n as f64 / total as f64) * plot_h };

    // Build SVG path
    let mut path = String::new();
    for (i, (t, n)) in trace.data_points.iter().enumerate() {
        let cmd = if i == 0 { "M" } else { "L" };
        path.push_str(&format!("{} {:.1} {:.1} ", cmd, x(*t), y(*n)));
    }

    // Fill area path
    let mut area_path = path.clone();
    if let Some((last_t, _)) = trace.data_points.last() {
        area_path.push_str(&format!("L {:.1} {:.1} ", x(*last_t), y(0)));
        area_path.push_str(&format!("L {:.1} {:.1} Z", x(0.0), y(0)));
    }

    // Grid lines
    let grid_lines = (0..=4)
        .map(|i| {
            let frac = i as f64 / 4.0;
            let ny = (total as f64 * frac) as usize;
            let ypos = y(ny);
            (ypos, ny)
        })
        .collect::<Vec<_>>();

    // Time labels
    let time_steps = 5;
    let time_labels: Vec<(f64, String)> = (0..=time_steps)
        .map(|i| {
            let t = max_time * i as f64 / time_steps as f64;
            let label = if t < 1000.0 {
                format!("{:.0}ms", t)
            } else {
                format!("{:.1}s", t / 1000.0)
            };
            (x(t), label)
        })
        .collect();

    let status_text = if trace.complete {
        let final_time = trace
            .data_points
            .iter()
            .find(|(_, n)| *n >= total)
            .map(|(t, _)| *t)
            .unwrap_or(max_time);
        if final_time < 1000.0 {
            format!("All {} nodes synced in {:.0}ms", total, final_time)
        } else {
            format!("All {} nodes synced in {:.1}s", total, final_time / 1000.0)
        }
    } else {
        let current = trace.data_points.last().map(|(_, n)| *n).unwrap_or(0);
        format!("Syncing: {}/{} nodes", current, total)
    };

    let status_color = if trace.complete {
        "text-emerald-400"
    } else {
        "text-amber-400"
    };

    rsx! {
        section { class: "mb-8",
            div { class: "flex items-center justify-between mb-4",
                h2 { class: "text-sm font-semibold uppercase tracking-widest text-slate-500", "Sync Propagation" }
                span { class: "text-sm font-medium {status_color}", "{status_text}" }
            }
            div { class: "bg-slate-900/80 border border-slate-800 rounded-xl p-5",
                svg {
                    width: "100%",
                    view_box: "0 0 {w} {h}",
                    class: "overflow-visible",

                    // Grid lines
                    for (ypos, ny) in &grid_lines {
                        line {
                            x1: "{pad_l}",
                            y1: "{ypos}",
                            x2: "{w - pad_r}",
                            y2: "{ypos}",
                            stroke: "#1e293b",
                            stroke_width: "1",
                        }
                        text {
                            x: "{pad_l - 8.0}",
                            y: "{ypos + 4.0}",
                            text_anchor: "end",
                            fill: "#475569",
                            font_size: "11",
                            "{ny}"
                        }
                    }

                    // Time labels
                    for (xpos, label) in &time_labels {
                        text {
                            x: "{xpos}",
                            y: "{h - 5.0}",
                            text_anchor: "middle",
                            fill: "#475569",
                            font_size: "11",
                            "{label}"
                        }
                    }

                    // Fill area
                    path {
                        d: "{area_path}",
                        fill: "rgba(139, 92, 246, 0.1)",
                    }

                    // Line
                    path {
                        d: "{path}",
                        fill: "none",
                        stroke: "#8b5cf6",
                        stroke_width: "2",
                        stroke_linejoin: "round",
                    }

                    // Target line (100%)
                    line {
                        x1: "{pad_l}",
                        y1: "{y(total)}",
                        x2: "{w - pad_r}",
                        y2: "{y(total)}",
                        stroke: "#10b981",
                        stroke_width: "1",
                        stroke_dasharray: "4,4",
                    }
                }
            }
        }
    }
}
