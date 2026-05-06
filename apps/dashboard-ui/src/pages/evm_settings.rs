use crate::api;
use crate::components::badge::{Badge, BadgeTone};
use dioxus::prelude::*;

#[component]
pub fn EvmSettings(bucket: String) -> Element {
    let mut evm = use_resource({
        let bucket = bucket.clone();
        move || {
            let bucket = bucket.clone();
            async move { api::buckets::get_evm_status(&bucket).await.ok() }
        }
    });

    // Fetch user's public ID for constructing the scoped RPC URL
    let profile = use_resource(|| async { api::developer::me().await.ok() });
    let user_id = profile
        .read()
        .as_ref()
        .and_then(|p| p.as_ref().map(|p| p.id.clone()))
        .unwrap_or_default();

    let chain_id = use_resource({
        let bucket = bucket.clone();
        let uid = user_id.clone();
        move || {
            let bucket = bucket.clone();
            let uid = uid.clone();
            async move {
                let url = format!("https://use1.api.shardd.xyz/evm/{uid}/{bucket}");
                let body = r#"{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}"#;
                let resp = gloo_net::http::Request::post(&url)
                    .header("Content-Type", "application/json")
                    .body(body)
                    .ok()?
                    .send()
                    .await
                    .ok()?;
                let text = resp.text().await.ok()?;
                let v: serde_json::Value = serde_json::from_str(&text).ok()?;
                let chain = v.get("result")?.as_str()?;
                let dec = u64::from_str_radix(chain.trim_start_matches("0x"), 16).ok()?;
                Some(dec.to_string())
            }
        }
    });

    let mut copied = use_signal(|| None::<usize>);

    let status = evm.read();
    let status_ref = status.as_ref().and_then(|s| s.as_ref());
    let enabled = status_ref.map(|s| s.enabled).unwrap_or(false);
    let paused = status_ref.map(|s| s.paused).unwrap_or(false);

    let badge_text = if enabled {
        if paused { "Paused" } else { "Active" }
    } else {
        "Disabled"
    };
    let badge_tone = if enabled && !paused { BadgeTone::Success } else { BadgeTone::Neutral };

    let edges: [(&str, &str); 3] = [
        ("US East", "https://use1.api.shardd.xyz"),
        ("EU Central", "https://euc1.api.shardd.xyz"),
        ("Asia Pacific", "https://ape1.api.shardd.xyz"),
    ];
    let rpc_path = format!("/evm/{user_id}/{bucket}");

    macro_rules! toggle {
        ($name:ident, $api:ident) => {
            let $name = {
                let bucket = bucket.clone();
                move |_| {
                    let bucket = bucket.clone();
                    spawn(async move {
                        let _ = api::buckets::$api(&bucket).await;
                        evm.restart();
                    });
                }
            };
        };
    }

    toggle!(on_enable, enable_evm);
    toggle!(on_disable, disable_evm);
    toggle!(on_pause, pause_evm);
    toggle!(on_resume, resume_evm);

    let chain_id_text = chain_id
        .read()
        .as_ref()
        .and_then(|s| s.as_ref())
        .map(|s| s.clone())
        .unwrap_or_else(|| "\u{2026}".to_string());

    rsx! {
        section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-5",
            div { class: "flex justify-between items-start",
                h2 { class: "text-[16px] font-normal", "EVM / Web3 RPC" }
                Badge { text: badge_text.to_string(), tone: badge_tone }
            }

            p { class: "font-mono text-[12px] text-base-500 leading-[140%]",
                "Expose this bucket as an EVM-compatible JSON-RPC endpoint. "
                "Wallets like MetaMask can read balances and send transfers "
                "without an API key \u{2014} the ECDSA signature on each "
                "transaction proves account ownership."
            }

            // ── RPC URLs for each edge ──────────────────────────────
            div { class: "rounded-lg border border-base-800 bg-base-1000 p-4 grid gap-3",
                span { class: "text-xs text-base-500 uppercase tracking-widest", "RPC Endpoints" }
                for edge_row in edges.iter().enumerate() {
                    {
                        let i = edge_row.0;
                        let label = edge_row.1.0;
                        let host = edge_row.1.1;
                        let url = format!("{host}{rpc_path}");
                        let is_copied = *copied.read() == Some(i);
                        rsx! {
                            div { class: "flex items-center justify-between gap-3",
                                div { class: "grid gap-0.5 flex-1 min-w-0",
                                    span { class: "text-[11px] text-base-500", "{label}" }
                                    code { class: "font-mono text-xs text-fg truncate block", "{url}" }
                                }
                                button {
                                    class: if is_copied {
                                        "shrink-0 px-3 py-1.5 rounded text-xs transition bg-green-950 text-green-400"
                                    } else {
                                        "shrink-0 px-3 py-1.5 rounded text-xs transition bg-base-800 hover:bg-base-700 text-fg"
                                    },
                                    onclick: {
                                        let url = url.clone();
                                        let j = i;
                                        move |_| {
                                            let url = url.clone();
                                            if let Some(w) = web_sys::window() {
                                                let _ = w.navigator().clipboard().write_text(&url);
                                            }
                                            copied.set(Some(j));
                                            let mut c = copied;
                                            spawn(async move {
                                                gloo_timers::future::TimeoutFuture::new(2000).await;
                                                c.set(None);
                                            });
                                        }
                                    },
                                    if is_copied { "Copied!" } else { "Copy" }
                                }
                            }
                        }
                    }
                }
            }

            // ── Chain ID card ──────────────────────────────────────
            if enabled {
                div { class: "rounded-lg border border-base-800 bg-base-1000 p-4 grid gap-3",
                    div { class: "grid grid-cols-2 gap-3",
                        div { class: "grid gap-1",
                            span { class: "text-xs text-base-500 uppercase tracking-widest", "Chain ID" }
                            code { class: "font-mono text-sm text-fg", "{chain_id_text}" }
                        }
                        div { class: "grid gap-1",
                            span { class: "text-xs text-base-500 uppercase tracking-widest", "Symbol" }
                            code { class: "font-mono text-sm text-fg", "SHARD" }
                        }
                    }
                }
            }

            // ── Enable / Disable ──────────────────────────────────
            div { class: "flex items-center justify-between",
                span { class: "text-sm text-fg",
                    if enabled { "EVM RPC is enabled" } else { "EVM RPC is disabled" }
                }
                if enabled {
                    button {
                        class: "px-4 py-2 rounded-full text-sm border border-red-800 text-red-400 hover:bg-red-950 transition",
                        onclick: on_disable,
                        "Disable EVM"
                    }
                } else {
                    button {
                        class: "px-4 py-2 rounded-full text-sm bg-[var(--btn-primary-bg)] hover:bg-base-800 text-[var(--btn-primary-text)] font-normal transition",
                        onclick: on_enable,
                        "Enable EVM"
                    }
                }
            }

            if enabled {
                div { class: "flex items-center justify-between",
                    span { class: "text-sm text-fg",
                        if paused { "Processing is paused" } else { "Processing is active" }
                    }
                    if paused {
                        button {
                            class: "px-3.5 py-1.5 rounded-full text-sm border border-base-700 text-base-400 hover:text-fg transition",
                            onclick: on_resume,
                            "Resume"
                        }
                    } else {
                        button {
                            class: "px-3.5 py-1.5 rounded-full text-sm border border-base-700 text-base-400 hover:text-fg transition",
                            onclick: on_pause,
                            "Pause"
                        }
                    }
                }
            }
        }
    }
}
