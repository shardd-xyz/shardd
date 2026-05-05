use crate::api;
use crate::components::badge::{Badge, BadgeTone};
use crate::state::use_notice;
use crate::types::{Notice, NoticeTone};
use dioxus::prelude::*;

/// EVM / Web3 settings tab shown on the bucket detail page.
///
/// Three independent toggles per bucket:
/// - **Enable** — turn the JSON-RPC endpoint on/off (off by default).
/// - **Pause** — temporarily reject writes while still serving reads.
/// - **Whitelist** — when on, only listed addresses may send transactions.
///
/// A "MetaMask config" card at the bottom shows the RPC URL and
/// chain ID so the user can copy them straight into their wallet.
#[component]
pub fn EvmSettings(bucket: String) -> Element {
    let mut evm = use_resource({
        let bucket = bucket.clone();
        move || {
            let bucket = bucket.clone();
            async move { api::buckets::get_evm_status(&bucket).await.ok() }
        }
    });
    let mut add_addr = use_signal(String::new);
    let mut notice = use_notice();

    let status = evm.read();
    let status_ref = status.as_ref().and_then(|s| s.as_ref());
    let enabled = status_ref.map(|s| s.enabled).unwrap_or(false);
    let paused = status_ref.map(|s| s.paused).unwrap_or(false);
    let whitelist = status_ref.map(|s| s.whitelist_enabled).unwrap_or(false);

    let badge_text = if enabled {
        if paused { "Paused" } else { "Active" }
    } else {
        "Disabled"
    };
    let badge_tone = if enabled && !paused {
        BadgeTone::Success
    } else {
        BadgeTone::Neutral
    };

    // The RPC URL path works on every edge. Append an API key with
    // write access to this bucket as a query parameter.
    let rpc_url = format!("https://use1.api.shardd.xyz/evm/{bucket}");

    // ── Closures ──────────────────────────────────────────────────

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
    toggle!(on_whitelist_enable, enable_evm_whitelist);
    toggle!(on_whitelist_disable, disable_evm_whitelist);

    let on_add_addr = {
        let bucket = bucket.clone();
        move |_| {
            let addr = add_addr.read().trim().to_string();
            if addr.is_empty() {
                return;
            }
            let bucket = bucket.clone();
            spawn(async move {
                match api::buckets::add_whitelist_address(&bucket, &addr).await {
                    Ok(_) => {
                        evm.restart();
                        add_addr.set(String::new());
                    }
                    Err(e) => {
                        notice.set(Some(Notice::new(
                            NoticeTone::Danger,
                            "Failed to add address",
                            e.friendly().1,
                        )));
                    }
                }
            });
        }
    };

    // ── Render ────────────────────────────────────────────────────

    rsx! {
        section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-5",
            div { class: "flex justify-between items-start",
                h2 { class: "text-[16px] font-normal", "EVM / Web3 RPC" }
                Badge { text: badge_text.to_string(), tone: badge_tone }
            }

            p { class: "font-mono text-[12px] text-base-500 leading-[140%]",
                "Expose this bucket as an EVM-compatible JSON-RPC endpoint. "
                "Wallets like MetaMask can read balances and send transfers. "
                "Each Ethereum address maps to a shardd account name. "
                "Transactions are signed ECDSA transfers verified by the edge."
            }

            // ── RPC URL card (always visible) ────────────────────
            div { class: "rounded-lg border border-base-800 bg-base-1000 p-4 grid gap-3",
                div { class: "grid gap-1",
                    span { class: "text-xs text-base-500 uppercase tracking-widest", "RPC URL" }
                    div { class: "flex items-center gap-2",
                        code { class: "flex-1 font-mono text-xs text-fg px-3 py-2 rounded bg-base-900 break-all", "{rpc_url}" }
                        button {
                            class: "px-3 py-2 rounded text-xs bg-base-800 hover:bg-base-700 text-fg transition",
                            onclick: {
                                let text = rpc_url.clone();
                                move |_| {
                                    if let Some(w) = web_sys::window() {
                                        let _ = w.navigator().clipboard().write_text(&text);
                                    }
                                }
                            },
                            "Copy"
                        }
                    }
                    p { class: "text-[11px] text-base-500 mt-1",
                        "Append \u{201C}?api_key=sk_live_\u{2026}\u{201D} with a key that has write scope on this bucket."
                    }
                }
                if enabled {
                    div { class: "grid grid-cols-2 gap-3",
                        div { class: "grid gap-1",
                            span { class: "text-xs text-base-500 uppercase tracking-widest", "Chain ID" }
                            code { class: "font-mono text-sm text-fg", "auto" }
                        }
                        div { class: "grid gap-1",
                            span { class: "text-xs text-base-500 uppercase tracking-widest", "Symbol" }
                            code { class: "font-mono text-sm text-fg", "SHARD" }
                        }
                    }
                    p { class: "text-[11px] text-base-500",
                        "Chain ID is derived deterministically from the bucket name. "
                        "Use the RPC URL above in MetaMask and call eth_chainId to get it."
                    }
                }
            }

            // ── Enable ───────────────────────────────────────────
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
                // ── Pause ────────────────────────────────────────
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

                // ── Whitelist toggle ─────────────────────────────
                div { class: "flex items-center justify-between",
                    span { class: "text-sm text-fg", "Address whitelist" }
                    if whitelist {
                        button {
                            class: "px-3.5 py-1.5 rounded-full text-sm border border-base-700 text-base-400 hover:text-fg transition",
                            onclick: on_whitelist_disable,
                            "Disable whitelist"
                        }
                    } else {
                        button {
                            class: "px-3.5 py-1.5 rounded-full text-sm border border-base-700 text-base-400 hover:text-fg transition",
                            onclick: on_whitelist_enable,
                            "Enable whitelist"
                        }
                    }
                }

                // ── Whitelisted addresses ─────────────────────────
                if whitelist {
                    if let Some(ref status) = status_ref {
                        div { class: "grid gap-2",
                            if status.addresses.is_empty() {
                                div { class: "text-sm text-base-500 py-2",
                                    "No addresses whitelisted. Add at least one to allow writes."
                                }
                            } else {
                                for addr in &status.addresses {
                                    div { class: "flex items-center justify-between bg-base-1000 rounded-lg px-4 py-2",
                                        code { class: "font-mono text-sm text-fg", "{addr.address}" }
                                        button {
                                            class: "text-red-400 hover:text-red-300 text-sm transition",
                                            onclick: {
                                                let bucket = bucket.clone();
                                                let a = addr.address.clone();
                                                move |_| {
                                                    let bucket = bucket.clone();
                                                    let a = a.clone();
                                                    spawn(async move {
                                                        let _ = api::buckets::remove_whitelist_address(
                                                            &bucket, &a,
                                                        )
                                                        .await;
                                                        evm.restart();
                                                    });
                                                }
                                            },
                                            "Remove"
                                        }
                                    }
                                }
                            }
                        }
                    }

                    form {
                        class: "flex gap-2",
                        onsubmit: on_add_addr,
                        input {
                            r#type: "text",
                            placeholder: "0xAbC123...",
                            class: "flex-1 px-3 py-2 rounded-lg bg-base-1000 border border-base-700 text-sm font-mono text-fg placeholder-base-500 focus:outline-none focus:border-accent-100",
                            value: "{add_addr}",
                            oninput: move |e| add_addr.set(e.value()),
                        }
                        button {
                            r#type: "submit",
                            class: "px-4 py-2 rounded-lg bg-[var(--btn-primary-bg)] hover:bg-base-800 text-[var(--btn-primary-text)] text-sm font-normal transition",
                            "Add"
                        }
                    }
                }
            }
        }
    }
}
