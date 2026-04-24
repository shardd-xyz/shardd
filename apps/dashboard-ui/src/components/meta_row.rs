use dioxus::prelude::*;

#[component]
pub fn MetaRow(label: String, value: String) -> Element {
    rsx! {
        div { class: "grid grid-cols-[100px_1fr] gap-4 py-2 border-b border-base-800 last:border-b-0 items-baseline",
            span { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "{label}" }
            span { class: "font-mono text-[14px] text-fg tracking-[-0.0175rem]", "{value}" }
        }
    }
}

#[component]
pub fn MetaRowCode(label: String, value: String) -> Element {
    rsx! {
        div { class: "grid grid-cols-[100px_1fr_auto] gap-4 py-2 border-b border-base-800 last:border-b-0 items-baseline",
            span { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "{label}" }
            span { class: "font-mono text-[14px] text-base-300 tracking-[-0.0175rem] break-all", "{value}" }
            crate::components::copy_button::CopyButton {
                value: value.clone(),
                label: Some("Copy".to_string()),
                on_copy: None,
            }
        }
    }
}
