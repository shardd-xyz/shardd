use crate::components::badge::{Badge, BadgeTone, event_type_badge};
use crate::components::time::*;
use crate::types::BucketEvent;
use dioxus::prelude::*;

#[derive(Clone, PartialEq, Props)]
pub struct EventCardProps {
    event: BucketEvent,
    #[props(default = true)]
    show_account: bool,
    #[props(default = false)]
    show_node: bool,
}

#[component]
pub fn EventCard(props: EventCardProps) -> Element {
    let event = &props.event;
    let event_type = if event.r#type.is_empty() {
        "standard"
    } else {
        &event.r#type
    };
    let hold_amount = event.hold_amount;
    let has_hold = hold_amount > 0;

    let (primary_amount, primary_class) = if event.amount == 0 && has_hold {
        (
            format!("-{}", format_amount(hold_amount as i64)),
            "text-accent-200",
        )
    } else {
        (
            format_signed_amount(event.amount),
            amount_class(event.amount),
        )
    };

    let hold_info = if has_hold {
        let expires = if event.hold_expires_at_unix_ms > 0 {
            format!(
                " · expires {}",
                format_relative_time(Some(event.hold_expires_at_unix_ms))
            )
        } else {
            String::new()
        };
        Some(format!(
            "{} held{expires}",
            format_amount(hold_amount as i64)
        ))
    } else {
        None
    };

    let is_void = event_type == "void" || event_type == "hold_release";
    let abs_time = format_date(Some(event.created_at_unix_ms));
    let rel_time = format_relative_time(Some(event.created_at_unix_ms));

    rsx! {
        article { class: "grid gap-2 p-3 rounded-lg border border-base-800 bg-base-900 hover:border-base-700 transition-colors duration-150",
            div { class: "flex flex-wrap items-center gap-2.5",
                span { class: "text-[18px] font-mono font-normal leading-none tracking-[-0.04em] {primary_class}", "{primary_amount}" }
                if props.show_account {
                    span { class: "font-mono text-[12px] px-1.5 py-0.5 rounded border border-base-800 bg-base-1000 text-base-300",
                        "{event.account}"
                    }
                }
                if event_type != "standard" {
                    {event_type_badge(event_type)}
                } else if has_hold {
                    Badge { text: "hold".to_string(), tone: BadgeTone::Warning }
                }
                time {
                    class: "ml-auto font-mono text-[12px] text-base-500 whitespace-nowrap",
                    title: "{abs_time}",
                    "{rel_time}"
                }
            }

            if let Some(hold) = &hold_info {
                span { class: "font-mono text-[12px] text-base-500", "{hold}" }
            }

            if let Some(note) = &event.note {
                if !note.is_empty() {
                    div { class: "font-mono text-[14px] text-base-300 whitespace-pre-wrap overflow-hidden leading-[140%] tracking-[-0.0175rem]",
                        style: "display: -webkit-box; -webkit-line-clamp: 3; -webkit-box-orient: vertical;",
                        "{note}"
                    }
                }
            }

            div { class: "flex flex-wrap items-center gap-2 text-base-600 font-mono text-[11px]",
                if props.show_node {
                    if let Some(node_id) = &event.origin_node_id {
                        span {
                            "node "
                            span { class: "px-1 py-0.5 rounded border border-base-800 bg-base-1000 text-base-400 max-w-[18ch] overflow-hidden text-ellipsis whitespace-nowrap inline-block align-bottom",
                                title: "{node_id}",
                                "{node_id}"
                            }
                        }
                    }
                }
                span {
                    "event "
                    span { class: "px-1 py-0.5 rounded border border-base-800 bg-base-1000 text-base-400",
                        "{event.event_id}"
                    }
                }
                if let Some(nonce) = &event.idempotency_nonce {
                    span {
                        "nonce "
                        span { class: "px-1 py-0.5 rounded border border-base-800 bg-base-1000 text-base-400",
                            "{nonce}"
                        }
                    }
                }
                if is_void {
                    if let Some(void_ref) = &event.void_ref {
                        span {
                            "ref "
                            span { class: "px-1 py-0.5 rounded border border-base-800 bg-base-1000 text-base-400",
                                "{void_ref}"
                            }
                        }
                    }
                }
            }
        }
    }
}
