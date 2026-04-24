use dioxus::prelude::*;

#[component]
pub fn StatCard(label: String, value: String, #[props(default)] title: String) -> Element {
    rsx! {
        article {
            class: "rounded-lg border border-base-800 bg-base-900 px-5 py-4",
            title: if !title.is_empty() { title.as_str() } else { "" },
            div { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500 mb-1", "{label}" }
            div { class: "text-[24px] font-mono font-normal leading-[100%] tracking-[-0.04em] text-fg", "{value}" }
        }
    }
}
