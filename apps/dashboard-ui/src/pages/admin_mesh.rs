use crate::api;
use crate::components::badge::{Badge, BadgeTone};
use crate::types::{MeshEdgeNodes, MeshNodeSummary};
use dioxus::prelude::*;

#[component]
pub fn AdminMesh() -> Element {
    let data = use_resource(|| async { api::admin::list_mesh_nodes().await.ok() });

    rsx! {
        div { class: "grid gap-6 w-full",
        section { class: "flex flex-wrap justify-between items-start gap-4",
            div { class: "grid gap-1",
                span { class: "text-accent-100 text-xs uppercase tracking-widest", "Operations" }
                h1 { class: "text-[32px] font-normal leading-[100%] tracking-[-0.04em] font-mono tracking-tight", "Mesh" }
            }
            if let Some(Some(edges)) = &*data.read() {
                Badge { text: format!("{} edges", edges.len()), tone: BadgeTone::Neutral }
            }
        }

        p { class: "text-base-500 font-mono text-[13px] leading-[140%]",
            "Each full-node advertises every multiaddr it's reachable on. libp2p dials "
            "them in parallel on new connections — same-region peers pick the AWS private "
            "IP, tailnet peers pick the 100.x address, everyone else falls back to public."
        }

        match &*data.read() {
            Some(Some(edges)) => rsx! {
                for edge in edges.iter() {
                    EdgeCard { edge: edge.clone() }
                }
            },
            Some(None) => rsx! { div { class: "text-base-500 text-center py-12", "Failed to load mesh nodes." } },
            None => rsx! { div { class: "text-base-500 text-center py-12", "Loading\u{2026}" } },
        }
        }
    }
}

#[component]
fn EdgeCard(edge: MeshEdgeNodes) -> Element {
    let mut expanded = use_signal(|| false);

    let title = edge.label.clone().unwrap_or_else(|| edge.edge_id.clone());
    let subtitle = format!("{} · {}", edge.region, edge.base_url);
    let best = edge.nodes.iter().find(|n| n.is_best);

    rsx! {
        section { class: "rounded-lg border border-base-800 bg-base-900 overflow-hidden",
            button {
                class: "w-full text-left p-6 grid gap-2 hover:bg-base-1000/40 transition-colors cursor-pointer",
                onclick: move |_| expanded.toggle(),
                div { class: "flex justify-between items-start flex-wrap gap-2",
                    div { class: "grid gap-1",
                        div { class: "flex items-center gap-2",
                            span { class: "text-base-500 font-mono text-[11px] uppercase tracking-widest",
                                if *expanded.read() { "\u{25bc}" } else { "\u{25b6}" }
                            }
                            h2 { class: "text-[16px] font-normal", "{title}" }
                            span { class: "text-base-500 font-mono text-[12px]", "({edge.edge_id})" }
                        }
                        span { class: "font-mono text-[12px] text-base-500 pl-6", "{subtitle}" }
                    }
                    div { class: "flex flex-wrap gap-2",
                        if let Some(err) = &edge.error {
                            Badge { text: format!("error: {err}"), tone: BadgeTone::Danger }
                        } else {
                            Badge { text: format!("{} nodes", edge.nodes.len()), tone: BadgeTone::Neutral }
                        }
                    }
                }
                if let Some(b) = best {
                    div { class: "flex items-center gap-2 pl-6",
                        Badge { text: "routes via".to_string(), tone: BadgeTone::Success }
                        span { class: "font-mono text-[13px] text-fg", "{node_title(b)}" }
                        if let Some(rtt) = b.ping_rtt_ms {
                            span { class: "font-mono text-[12px] text-base-500", "{rtt} ms" }
                        }
                    }
                }
            }
            if *expanded.read() {
                div { class: "border-t border-base-800 p-6 grid gap-3",
                    if edge.nodes.is_empty() && edge.error.is_none() {
                        div { class: "text-base-500 font-mono text-[13px]", "No nodes discovered." }
                    }
                    for node in edge.nodes.iter() {
                        NodeCard { node: node.clone() }
                    }
                }
            }
        }
    }
}

#[component]
fn NodeCard(node: MeshNodeSummary) -> Element {
    let mut expanded = use_signal(|| false);

    let (ready_tone, ready_text) = match node.ready {
        Some(true) => (BadgeTone::Success, "ready"),
        Some(false) => (BadgeTone::Warning, "not ready"),
        None => (BadgeTone::Neutral, "unknown"),
    };
    // The addr the gateway is most likely routing over. libp2p doesn't surface
    // the live connection path; we approximate by the same preference order
    // libp2p's happy-eyeballs dial tends to pick: private > tailscale > public.
    let preferred = pick_preferred_addr(&node.listen_addrs);

    let border = if node.is_best {
        "border-accent-100/50"
    } else {
        "border-base-800"
    };

    rsx! {
        div { class: "rounded-lg border bg-base-1000/20 overflow-hidden {border}",
            button {
                class: "w-full text-left p-4 grid gap-2 hover:bg-base-1000/40 transition-colors cursor-pointer",
                onclick: move |_| expanded.toggle(),
                div { class: "flex flex-wrap justify-between items-start gap-2",
                    div { class: "grid gap-1",
                        div { class: "flex items-center gap-2 flex-wrap",
                            span { class: "text-base-500 font-mono text-[11px]",
                                if *expanded.read() { "\u{25bc}" } else { "\u{25b6}" }
                            }
                            span { class: "font-mono text-[14px] text-fg", "{node_title(&node)}" }
                            if node.is_best {
                                Badge { text: "best".to_string(), tone: BadgeTone::Success }
                            }
                        }
                        span { class: "font-mono text-[11px] text-base-500 pl-5", "{node.node_id}" }
                    }
                    div { class: "flex flex-wrap gap-2",
                        Badge { text: ready_text.to_string(), tone: ready_tone }
                        if let Some(rtt) = node.ping_rtt_ms {
                            Badge { text: format!("{rtt} ms"), tone: BadgeTone::Neutral }
                        }
                        if node.failure_count > 0 {
                            Badge { text: format!("{} failures", node.failure_count), tone: BadgeTone::Warning }
                        }
                    }
                }
                if let Some((addr, label, tone)) = preferred.as_ref() {
                    div { class: "flex items-center gap-2 pl-5",
                        Badge { text: format!("via {label}"), tone: tone.clone() }
                        code { class: "font-mono text-[12px] text-base-400 truncate", "{addr}" }
                    }
                }
            }
            if *expanded.read() {
                div { class: "border-t border-base-800 p-4 grid gap-1",
                    span { class: "font-mono text-[11px] uppercase tracking-widest text-base-500 pb-1",
                        "all advertised addresses"
                    }
                    if node.listen_addrs.is_empty() {
                        div { class: "text-base-500 font-mono text-[12px]", "None yet." }
                    }
                    for addr in node.listen_addrs.iter() {
                        AddrRow { addr: addr.clone() }
                    }
                }
            }
        }
    }
}

#[component]
fn AddrRow(addr: String) -> Element {
    let (label, tone) = classify_addr(&addr);
    rsx! {
        div { class: "flex items-center gap-2",
            Badge { text: label.to_string(), tone }
            code { class: "font-mono text-[12px] text-base-400", "{addr}" }
        }
    }
}

fn node_title(node: &MeshNodeSummary) -> String {
    match &node.label {
        Some(lbl) => lbl.clone(),
        None => short_id(&node.node_id),
    }
}

fn short_id(s: &str) -> String {
    // "73119020-8083-4086-b826-e1d3d8bb3468" → "73119020"
    s.split('-').next().unwrap_or(s).to_string()
}

fn pick_preferred_addr(addrs: &[String]) -> Option<(String, &'static str, BadgeTone)> {
    // Libp2p's real connection is invisible here, but it reliably picks the
    // first that dials successfully, and within a VPC the private IP dial
    // wins the happy-eyeballs race. Mirror that preference.
    //
    // Skip loopback (127.x) and docker bridge (/p2p/ suffixed forms — noisy).
    let iter = addrs
        .iter()
        .filter(|a| !a.contains("/p2p/"))
        .filter(|a| !a.starts_with("/ip4/127."));
    // Pass 1: AWS private (non-docker: 172.31 or 10.x)
    for a in iter.clone() {
        if is_aws_private(a) {
            return Some((a.clone(), "private", BadgeTone::Primary));
        }
    }
    for a in iter.clone() {
        if is_tailscale(a) {
            return Some((a.clone(), "tailscale", BadgeTone::Success));
        }
    }
    for a in iter.clone() {
        if is_rfc1918(a) {
            return Some((a.clone(), "private", BadgeTone::Primary));
        }
    }
    iter.clone()
        .next()
        .cloned()
        .map(|a| (a, "public", BadgeTone::Neutral))
}

fn classify_addr(addr: &str) -> (&'static str, BadgeTone) {
    if let Some(octets) = extract_ip4(addr) {
        let (a, b) = (octets[0], octets[1]);
        if a == 100 && (64..=127).contains(&b) {
            ("tailscale", BadgeTone::Success)
        } else if is_rfc1918_octets(octets) {
            ("private", BadgeTone::Primary)
        } else if a == 127 {
            ("loopback", BadgeTone::Neutral)
        } else {
            ("public", BadgeTone::Neutral)
        }
    } else {
        ("dns", BadgeTone::Neutral)
    }
}

fn is_aws_private(addr: &str) -> bool {
    // AWS default VPC uses 172.31/16; our tf state shows 172.31.x.x. Docker
    // bridge uses 172.17-172.18 so we exclude those.
    let Some(octets) = extract_ip4(addr) else {
        return false;
    };
    (octets[0] == 172 && octets[1] == 31) || octets[0] == 10
}

fn is_tailscale(addr: &str) -> bool {
    extract_ip4(addr)
        .map(|o| o[0] == 100 && (64..=127).contains(&o[1]))
        .unwrap_or(false)
}

fn is_rfc1918(addr: &str) -> bool {
    extract_ip4(addr).map(is_rfc1918_octets).unwrap_or(false)
}

fn is_rfc1918_octets(o: [u8; 4]) -> bool {
    o[0] == 10 || (o[0] == 172 && (16..=31).contains(&o[1])) || (o[0] == 192 && o[1] == 168)
}

fn extract_ip4(addr: &str) -> Option<[u8; 4]> {
    let rest = addr.strip_prefix("/ip4/")?;
    let ip = rest.split('/').next()?;
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut out = [0u8; 4];
    for (i, p) in parts.iter().enumerate() {
        out[i] = p.parse().ok()?;
    }
    Some(out)
}
