use crate::router::Route;
use dioxus::prelude::*;

#[component]
pub fn NotFound(segments: Vec<String>) -> Element {
    let path = format!("/{}", segments.join("/"));
    let nav = use_navigator();

    if path == "/" {
        nav.push(Route::Dashboard);
        return rsx! {};
    }

    rsx! {
        div { class: "min-h-screen flex items-center justify-center px-4",
            div { class: "grid gap-2 text-center",
                h1 { class: "text-[32px] font-normal leading-[100%] tracking-[-0.04em] font-mono tracking-tight", "Not found" }
                p { class: "text-base-500", "The path {path} does not exist." }
            }
        }
    }
}
