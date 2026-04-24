use dioxus::prelude::*;

#[derive(Clone, PartialEq)]
pub enum BadgeTone {
    Neutral,
    Primary,
    Success,
    Warning,
    Danger,
}

impl BadgeTone {
    fn classes(&self) -> &'static str {
        match self {
            Self::Neutral => "border-base-700 bg-base-900 text-base-500",
            Self::Primary => "border-accent-100/30 bg-base-900 text-accent-100",
            Self::Success => "border-accent-100/30 bg-base-900 text-accent-100",
            Self::Warning => "border-accent-200/30 bg-base-900 text-accent-200",
            Self::Danger => "border-[#f87171]/20 bg-base-900 text-[#f87171]",
        }
    }
}

#[component]
pub fn Badge(text: String, tone: BadgeTone) -> Element {
    let classes = tone.classes();
    rsx! {
        span { class: "inline-flex items-center px-2 py-0.5 rounded font-mono text-[11px] uppercase tracking-[-0.01rem] border {classes}",
            "{text}"
        }
    }
}

pub fn event_type_badge(event_type: &str) -> Element {
    let text: String = match event_type {
        "reservation_create" => "reservation".to_string(),
        "void" => "void".to_string(),
        "hold_release" => "release".to_string(),
        "standard" => "standard".to_string(),
        other => other.to_string(),
    };
    let tone = match event_type {
        "reservation_create" => BadgeTone::Warning,
        "void" => BadgeTone::Danger,
        "hold_release" => BadgeTone::Success,
        "standard" => BadgeTone::Primary,
        _ => BadgeTone::Neutral,
    };
    rsx! { Badge { text: text, tone: tone } }
}
