use dioxus::prelude::*;

#[component]
pub fn Header(
    bootstrap_url: Signal<String>,
    on_connect: EventHandler<String>,
    connected: bool,
) -> Element {
    let mut input_val = use_signal(|| bootstrap_url.read().clone());

    rsx! {
        header { class: "flex flex-col sm:flex-row items-start sm:items-center justify-between gap-4 pb-6 mb-8 border-b border-slate-800",
            div { class: "flex items-center gap-3",
                div { class: "w-8 h-8 rounded-lg bg-gradient-to-br from-violet-500 to-cyan-500 flex items-center justify-center",
                    span { class: "text-white text-sm font-bold", "S" }
                }
                h1 { class: "text-xl font-semibold tracking-tight text-white",
                    "shardd"
                    span { class: "text-slate-500 font-normal ml-2", "dashboard" }
                }
            }
            div { class: "flex items-center gap-3",
                input {
                    r#type: "text",
                    class: "bg-slate-900 border border-slate-700 text-slate-300 text-sm rounded-lg px-4 py-2 w-72 focus:outline-none focus:ring-2 focus:ring-violet-500/50 focus:border-violet-500 placeholder-slate-600 transition-all",
                    placeholder: "http://host:3001",
                    value: "{input_val}",
                    oninput: move |e| input_val.set(e.value()),
                    onkeypress: move |e| {
                        if e.key() == Key::Enter {
                            on_connect.call(input_val.read().clone());
                        }
                    },
                }
                button {
                    class: "bg-violet-600 hover:bg-violet-500 text-white text-sm font-medium px-5 py-2 rounded-lg transition-colors",
                    onclick: move |_| on_connect.call(input_val.read().clone()),
                    "Connect"
                }
                if connected {
                    div { class: "flex items-center gap-2 text-xs",
                        div { class: "w-2 h-2 rounded-full bg-emerald-400 animate-pulse-dot" }
                        span { class: "text-emerald-400", "Live" }
                    }
                } else {
                    div { class: "flex items-center gap-2 text-xs",
                        div { class: "w-2 h-2 rounded-full bg-slate-600" }
                        span { class: "text-slate-500", "Offline" }
                    }
                }
            }
        }
    }
}
