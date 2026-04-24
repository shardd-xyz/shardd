mod api;
mod components;
mod pages;
mod router;
mod state;
mod types;

use dioxus::prelude::*;
use router::Route;
use types::{FlashKey, Notice, Session};

fn main() {
    // Capture the initial URL before the router mounts and potentially
    // normalizes the location (stripping query strings like ?token= on
    // routes that don't declare them). Pages that need the original query
    // (notably /magic) can read this static.
    let initial_search = web_sys::window()
        .and_then(|w| w.location().search().ok())
        .unwrap_or_default();
    let _ = state::INITIAL_SEARCH.set(initial_search);
    launch(app);
}

fn app() -> Element {
    use_context_provider(|| Signal::new(None::<Session>));
    use_context_provider(|| Signal::new(None::<Notice>));
    use_context_provider(|| Signal::new(None::<FlashKey>));

    rsx! {
        document::Link { rel: "stylesheet", href: asset!("../assets/tailwind.css") }
        script { r#defer: true, src: "https://cloud.umami.is/script.js", "data-website-id": "81c87671-2492-478d-9e60-12c3b0696fad" }
        Router::<Route> {}
    }
}
