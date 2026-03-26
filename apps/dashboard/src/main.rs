mod api;
mod components;

use dioxus::prelude::*;
use shardd_types::{Event, StateResponse};

use components::events::EventSection;
use components::header::Header;
use components::nodes::NodeList;
use components::overview::Overview;
use components::sync_chart::{SyncChart, SyncTrace};

fn main() {
    dioxus::launch(app);
}

fn app() -> Element {
    let bootstrap_url = use_signal(|| String::new());
    let mut connected = use_signal(|| false);
    let mut node_urls = use_signal(Vec::<String>::new);
    let mut node_states = use_signal(Vec::<(String, StateResponse)>::new);
    let mut events = use_signal(Vec::<Event>::new);
    let mut poll_tick = use_signal(|| 0u64);
    let sync_trace = use_signal(|| SyncTrace {
        total_nodes: 0,
        target_event_count: 0,
        data_points: vec![],
        complete: false,
    });

    // Periodic polling (only ticks when connected)
    use_effect(move || {
        spawn(async move {
            loop {
                gloo_timers::future::sleep(std::time::Duration::from_secs(2)).await;
                poll_tick += 1;
            }
        });
    });

    // Fetch state from all nodes on tick
    use_effect(move || {
        let _tick = poll_tick.read();
        let urls: Vec<String> = node_urls.read().clone();
        if urls.is_empty() {
            return;
        }
        spawn(async move {
            let states = api::fetch_all_states(&urls).await;
            if !states.is_empty() {
                node_states.set(states);
            }

            if let Some(url) = urls.first() {
                if let Ok(evts) = api::fetch_events(url).await {
                    events.set(evts);
                }
            }

            // Re-discover peers
            if let Some(url) = urls.first() {
                if let Ok(discovered) = api::discover_nodes(url).await {
                    let mut current = node_urls.read().clone();
                    let mut changed = false;
                    for u in discovered {
                        if !current.contains(&u) {
                            current.push(u);
                            changed = true;
                        }
                    }
                    if changed {
                        current.sort();
                        node_urls.set(current);
                    }
                }
            }
        });
    });

    let on_connect = move |url: String| {
        spawn(async move {
            match api::discover_nodes(&url).await {
                Ok(urls) => {
                    node_urls.set(urls);
                    connected.set(true);
                    poll_tick += 1;
                }
                Err(_) => {
                    connected.set(false);
                    node_urls.set(vec![]);
                    node_states.set(vec![]);
                }
            }
        });
    };

    let states_snapshot = node_states.read().clone();
    let events_snapshot = events.read().clone();
    let urls_snapshot = node_urls.read().clone();
    let trace_snapshot = sync_trace.read().clone();

    rsx! {
        div { class: "min-h-screen bg-slate-950 text-slate-200 font-mono",
            div { class: "max-w-7xl mx-auto px-4 sm:px-6 lg:px-8 py-6",
                Header {
                    bootstrap_url,
                    on_connect,
                    connected: *connected.read(),
                }
                Overview { states: states_snapshot.clone() }
                SyncChart { trace: trace_snapshot }
                NodeList { states: states_snapshot, urls: urls_snapshot.clone() }
                EventSection { node_urls: urls_snapshot, events: events_snapshot, sync_trace }
            }
        }
    }
}
