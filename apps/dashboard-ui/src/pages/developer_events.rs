use crate::pages::admin_events::{EventsScope, EventsView};
use dioxus::prelude::*;

#[component]
pub fn Events() -> Element {
    rsx! { EventsView { scope: EventsScope::Developer } }
}
