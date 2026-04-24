use crate::types::{EdgeHealth, EdgeInfo};
use dioxus::prelude::*;

#[derive(Clone)]
struct EdgeState {
    info: EdgeInfo,
    health: Option<EdgeHealth>,
    client_rtt_ms: Option<f64>,
    status: &'static str,
}

#[component]
pub fn EdgeMeshStatus(edges: Vec<EdgeInfo>) -> Element {
    let mut edge_states: Signal<Vec<EdgeState>> = use_signal(|| {
        edges
            .iter()
            .map(|e| EdgeState {
                info: e.clone(),
                health: None,
                client_rtt_ms: None,
                status: "loading",
            })
            .collect()
    });

    use_effect(move || {
        spawn(async move {
            loop {
                let current = edge_states.read().clone();
                let mut updated = Vec::new();
                for es in &current {
                    let url = format!("{}/gateway/health", es.info.base_url.trim_end_matches('/'));
                    let start = js_sys::Date::now();
                    let result = gloo_net::http::Request::get(&url).send().await;
                    let rtt = js_sys::Date::now() - start;
                    match result {
                        Ok(res) if res.ok() => {
                            if let Ok(health) = res.json::<EdgeHealth>().await {
                                let status = if health.overloaded == Some(true) {
                                    "degraded"
                                } else if health.ready {
                                    "healthy"
                                } else {
                                    "offline"
                                };
                                updated.push(EdgeState {
                                    info: es.info.clone(),
                                    health: Some(health),
                                    client_rtt_ms: Some(rtt),
                                    status,
                                });
                                continue;
                            }
                        }
                        _ => {}
                    }
                    updated.push(EdgeState {
                        info: es.info.clone(),
                        health: None,
                        client_rtt_ms: None,
                        status: "offline",
                    });
                }
                edge_states.set(updated);
                gloo_timers::future::TimeoutFuture::new(3000).await;
            }
        });
    });

    let states = edge_states.read();
    let healthy = states.iter().filter(|s| s.status == "healthy").count();
    let total = states.len();
    let mesh_label = if healthy == total && total > 0 {
        "live"
    } else if healthy > 0 {
        "degraded"
    } else {
        "offline"
    };

    rsx! {
        section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-4",
            div { class: "flex justify-between items-center",
                h2 { class: "text-[16px] font-mono font-normal text-fg", "Edge mesh" }
                div { class: "flex items-center gap-2",
                    {
                        let dot = match mesh_label { "live" => "bg-accent-100", "degraded" => "bg-[#fbbf24]", _ => "bg-base-600" };
                        rsx! { span { class: "inline-block w-2 h-2 rounded-full {dot}" } }
                    }
                    span { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500",
                        "{healthy}/{total} · {mesh_label}"
                    }
                }
            }
            div { class: "grid gap-2",
                for es in states.iter() {
                    {
                        let dot_color = match es.status {
                            "healthy" => "bg-accent-100",
                            "degraded" => "bg-[#fbbf24]",
                            _ => "bg-base-600",
                        };
                        let rtt_text = match es.client_rtt_ms {
                            Some(ms) if ms < 1.0 => "<1 ms".to_string(),
                            Some(ms) => format!("{} ms", ms.round() as u64),
                            None => "—".to_string(),
                        };
                        let mesh_rtt = es.health.as_ref().and_then(|h| h.best_node_rtt_ms).map(|ms| {
                            if ms == 0 { "mesh <1 ms".to_string() } else { format!("mesh {} ms", ms) }
                        });
                        let nodes = es.health.as_ref().map(|h| format!("{} nodes", h.healthy_nodes)).unwrap_or_else(|| "—".to_string());
                        let sync = es.health.as_ref().and_then(|h| h.sync_gap).map(|g| format!("gap {g}")).unwrap_or_default();

                        rsx! {
                            div { class: "flex items-center gap-3 p-3 rounded-lg border border-base-800 bg-base-1000",
                                span { class: "inline-block w-2 h-2 rounded-full {dot_color} flex-shrink-0" }
                                div { class: "grid gap-0.5 flex-1 min-w-0",
                                    div { class: "flex items-center gap-2",
                                        {
                                            let edge_display = es.info.label.as_deref().unwrap_or(&es.info.edge_id);
                                            rsx! { span { class: "font-mono text-[14px] text-fg", "{edge_display}" } }
                                        }
                                        span { class: "font-mono text-[11px] text-base-500", "{es.info.region}" }
                                        if let Some(mr) = &mesh_rtt {
                                            span { class: "font-mono text-[11px] text-base-600", "· {mr}" }
                                        }
                                    }
                                    div { class: "flex items-center gap-2 font-mono text-[11px] text-base-600",
                                        span { "{nodes}" }
                                        if !sync.is_empty() {
                                            span { "· {sync}" }
                                        }
                                    }
                                }
                                div { class: "text-right flex-shrink-0",
                                    span { class: "font-mono text-[16px] text-fg", "{rtt_text}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
