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

    let status = evm.read();
    let status_ref = status.as_ref().and_then(|s| s.as_ref());
    let enabled = status_ref.map(|s| s.enabled).unwrap_or(false);
    let paused = status_ref.map(|s| s.paused).unwrap_or(false);

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

    let rpc_url = format!("https://use1.api.shardd.xyz/evm/{bucket}");

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
                        "Chain ID is derived from the bucket name. "
                        "Use the RPC URL in MetaMask and call eth_chainId to get it."
                    }
                }
            }

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
