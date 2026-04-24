use crate::router::Route;
use dioxus::prelude::*;

/// Reusable empty-state card. Replaces scattered one-liners like
/// "No buckets yet." with a tidy block that points at the next action.
///
/// Either `cta` (internal route) or `external_cta` (href opened in a new tab)
/// may be set, not both.
#[component]
pub fn EmptyState(
    title: String,
    body: String,
    cta: Option<(String, Route)>,
    external_cta: Option<(String, String)>,
) -> Element {
    rsx! {
        div { class: "rounded-lg border border-dashed border-base-700 bg-base-900 p-8 grid gap-3 text-center",
            strong { class: "font-mono text-[14px] text-fg", "{title}" }
            p { class: "font-mono text-[13px] text-base-500 leading-[140%] m-0", "{body}" }
            if let Some((label, route)) = cta.clone() {
                div { class: "flex justify-center",
                    Link {
                        to: route,
                        class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-accent-100 hover:text-fg transition-colors duration-150 no-underline",
                        "{label} \u{2192}"
                    }
                }
            } else if let Some((label, href)) = external_cta.clone() {
                div { class: "flex justify-center",
                    a {
                        href: "{href}",
                        target: "_blank",
                        rel: "noopener",
                        class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-accent-100 hover:text-fg transition-colors duration-150 no-underline",
                        "{label} \u{2197}"
                    }
                }
            }
        }
    }
}
