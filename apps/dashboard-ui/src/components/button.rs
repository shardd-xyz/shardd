use dioxus::prelude::*;

#[allow(dead_code)]
#[derive(Clone, PartialEq)]
pub enum BtnVariant {
    Primary,
    Secondary,
    Danger,
    Ghost,
}

#[allow(dead_code)]
#[derive(Clone, PartialEq)]
pub enum BtnSize {
    Sm,
    Default,
    Lg,
}

#[component]
pub fn Btn(
    #[props(default = BtnVariant::Primary)] variant: BtnVariant,
    #[props(default = BtnSize::Default)] size: BtnSize,
    #[props(default = false)] disabled: bool,
    #[props(default)] onclick: Option<EventHandler<MouseEvent>>,
    #[props(default = "button".to_string())] r#type: String,
    /// Extra utility classes appended last. Useful to override height
    /// (e.g. `h-[38px]` to match a sibling input) without adding a new size
    /// variant for every edge case.
    #[props(default = String::new())]
    class: String,
    children: Element,
) -> Element {
    let base = "group relative inline-flex w-max items-center justify-center border font-mono uppercase transition-colors duration-150 disabled:cursor-not-allowed disabled:opacity-50 overflow-hidden";

    let variant_cls = match variant {
        BtnVariant::Primary => {
            "bg-[var(--btn-primary-bg)] text-[var(--btn-primary-text)] border-base-600 hover:opacity-80"
        }
        BtnVariant::Secondary => {
            "bg-fg text-[var(--background)] border-transparent hover:opacity-80"
        }
        BtnVariant::Danger => "bg-fg text-[var(--background)] border-transparent hover:opacity-80",
        BtnVariant::Ghost => {
            "bg-transparent text-base-400 border-base-700 hover:text-fg hover:border-base-600"
        }
    };

    let size_cls = match size {
        BtnSize::Sm => "h-[25px] px-3 text-[12px] tracking-[-0.015rem] rounded-sm",
        BtnSize::Default => "h-[31px] px-[14px] text-[12px] tracking-[-0.015rem] rounded-sm",
        BtnSize::Lg => "h-[40px] px-6 text-[14px] tracking-[-0.0175rem] rounded-sm",
    };

    let show_stripe = matches!(
        variant,
        BtnVariant::Primary | BtnVariant::Secondary | BtnVariant::Danger
    );

    rsx! {
        button {
            class: "{base} {variant_cls} {size_cls} {class}",
            r#type: "{r#type}",
            disabled: disabled,
            onclick: move |evt| { if let Some(handler) = &onclick { handler.call(evt); } },
            if show_stripe {
                div { class: "pointer-events-none absolute inset-0 opacity-0 group-hover:opacity-100 transition-opacity duration-100",
                    div { class: "btn-stripe-pattern absolute inset-0" }
                }
            }
            span { class: "relative z-10 flex items-center gap-1",
                {children}
            }
        }
    }
}
