use dioxus::prelude::*;

#[component]
pub fn Pagination(
    total: usize,
    page: usize,
    total_pages: usize,
    label: String,
    #[props(default = false)] loading: bool,
    on_prev: EventHandler<()>,
    on_next: EventHandler<()>,
) -> Element {
    let prev_cls = if page > 1 && !loading {
        "px-2 py-0.5 rounded border border-base-700 text-base-400 hover:text-fg uppercase tracking-[-0.015rem] transition-colors duration-150"
    } else {
        "px-2 py-0.5 rounded border border-base-800 text-base-700 uppercase tracking-[-0.015rem] cursor-default"
    };
    let next_cls = if page < total_pages && !loading {
        "px-2 py-0.5 rounded border border-base-700 text-base-400 hover:text-fg uppercase tracking-[-0.015rem] transition-colors duration-150"
    } else {
        "px-2 py-0.5 rounded border border-base-800 text-base-700 uppercase tracking-[-0.015rem] cursor-default"
    };

    rsx! {
        section { class: "flex flex-wrap items-center gap-3 font-mono text-[12px] text-base-500 mt-2",
            span { "{total} {label} · page {page} / {total_pages}" }
            if loading {
                span { class: "inline-block w-3 h-3 border-2 border-base-700 border-t-accent-100 rounded-full animate-spin" }
            }
            span { class: "flex-1" }
            button {
                class: "{prev_cls}",
                disabled: page <= 1 || loading,
                onclick: move |_| if page > 1 && !loading { on_prev.call(()) },
                "Prev"
            }
            button {
                class: "{next_cls}",
                disabled: page >= total_pages || loading,
                onclick: move |_| if page < total_pages && !loading { on_next.call(()) },
                "Next"
            }
        }
    }
}
