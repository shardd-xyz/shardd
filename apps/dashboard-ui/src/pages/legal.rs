use dioxus::prelude::*;

fn legal_footer() -> Element {
    rsx! {
        footer { class: "border-t border-base-800 mt-auto",
            div { class: "mx-auto max-w-[680px] px-4 py-6 flex flex-wrap items-center gap-x-6 gap-y-2 font-mono text-[12px] text-base-600",
                span { "© 2026 TQDM Inc." }
                a { href: "/tos", class: "text-base-500 hover:text-fg transition-colors duration-150 no-underline", "Terms" }
                a { href: "/privacy", class: "text-base-500 hover:text-fg transition-colors duration-150 no-underline", "Privacy" }
                span { class: "flex-1" }
                a { href: "/login", class: "text-base-500 hover:text-fg transition-colors duration-150 no-underline", "Sign in" }
            }
        }
    }
}

fn section(title: &str, points: &[&str]) -> Element {
    rsx! {
        div { class: "grid gap-2",
            h2 { class: "text-[16px] text-fg mt-2 font-mono", "{title}" }
            ul { class: "grid gap-2 list-disc pl-5",
                for point in points {
                    li { "{point}" }
                }
            }
        }
    }
}

#[component]
pub fn Tos() -> Element {
    rsx! {
        div { class: "min-h-screen flex flex-col",
            div { class: "flex-1 px-4 py-12",
                div { class: "mx-auto max-w-[680px] grid gap-6",
                    div { class: "flex items-center gap-3 mb-4",
                        svg { width: "28", height: "28", view_box: "0 0 128 128", fill: "none",
                            rect { width: "128", height: "128", rx: "24", fill: "#12202C" }
                            circle { cx: "34", cy: "34", r: "10", fill: "#0F8B8D" }
                            circle { cx: "94", cy: "34", r: "10", fill: "#E85D04" }
                            circle { cx: "64", cy: "94", r: "10", fill: "#F4D35E" }
                            path { d: "M34 34L94 34L64 94L34 34Z", stroke: "#F7F3EC", stroke_width: "8", stroke_linejoin: "round" }
                        }
                        span { class: "font-mono text-[14px] text-fg", "shardd" }
                    }
                    h1 { class: "text-[32px] font-mono font-normal leading-[100%] tracking-[-0.04em] text-fg", "Terms of Service" }
                    div { class: "grid gap-4 font-mono text-[14px] text-base-400 leading-[160%]",

                        {section("1. Service description", &[
                            "shardd is an online platform operated by TQDM Inc. that provides a distributed ledger control plane, enabling users to manage buckets, events, API keys, and account balances across a mesh network of nodes.",
                            "The service is provided as-is without guarantee of continuous availability. shardd is not a financial institution and does not hold, transfer, or manage real currency.",
                        ])}

                        {section("2. Operator", &[
                            "TQDM Inc., 1111B S Governors Ave, STE 23256, Dover, DE 19904, USA",
                            "Email: contact@tqdm.org | Phone: +1 (814) 524-5685",
                        ])}

                        {section("3. Accounts and authentication", &[
                            "Sign-in is completed via email magic link or Google OAuth. You are responsible for keeping your email inbox and devices secure.",
                            "You may not share API keys or allow unauthorized access to your account.",
                            "We may suspend or terminate accounts that violate these terms or engage in abusive behavior.",
                        ])}

                        {section("4. Acceptable use", &[
                            "You agree not to misuse the service, interfere with its operation, attempt to access other users' data, or use it for any unlawful purpose.",
                            "Automated access is permitted only through issued API keys within their configured scopes.",
                        ])}

                        {section("5. Data and the mesh network", &[
                            "Bucket data is distributed across the shardd mesh network, which may span multiple geographic regions.",
                            "We do not guarantee permanent retention of data. You are responsible for maintaining your own backups where needed.",
                            "Event data, once written to the ledger, is replicated across nodes and may not be individually deletable from all replicas.",
                        ])}

                        {section("6. Warranty and availability", &[
                            "No guarantee is given for the accuracy, completeness, timeliness, or continuous availability of the service.",
                            "Node outages, network partitions, or maintenance may temporarily affect data availability or write latency.",
                            "shardd is infrastructure tooling and does not replace professional financial, legal, or compliance advice.",
                        ])}

                        {section("7. Liability", &[
                            "We are fully liable for damages resulting from injury to life, body, or health, and for damages caused by intent or gross negligence.",
                            "For minor negligent breaches of essential contractual obligations, liability is limited to typical and foreseeable damages.",
                            "Otherwise, liability for minor negligence is excluded. Mandatory statutory liability remains unaffected.",
                        ])}

                        {section("8. Changes and termination", &[
                            "You may delete your account at any time from the Profile page. This removes your user record and revokes all API keys.",
                            "We may modify or discontinue the service or individual features at any time with effect for the future.",
                            "We will inform you appropriately about material changes to these terms.",
                        ])}

                        {section("9. Governing law", &[
                            "These terms are governed by the laws of the State of Delaware, USA.",
                            "Mandatory consumer protection provisions of your jurisdiction remain unaffected.",
                        ])}

                        {section("10. Contact", &[
                            "TQDM Inc., 1111B S Governors Ave, STE 23256, Dover, DE 19904, USA",
                            "Email: contact@tqdm.org | Phone: +1 (814) 524-5685",
                        ])}

                        p { class: "text-base-600 mt-4", "Last updated: April 2026" }
                    }
                }
            }
            {legal_footer()}
        }
    }
}

#[component]
pub fn Privacy() -> Element {
    rsx! {
        div { class: "min-h-screen flex flex-col",
            div { class: "flex-1 px-4 py-12",
                div { class: "mx-auto max-w-[680px] grid gap-6",
                    div { class: "flex items-center gap-3 mb-4",
                        svg { width: "28", height: "28", view_box: "0 0 128 128", fill: "none",
                            rect { width: "128", height: "128", rx: "24", fill: "#12202C" }
                            circle { cx: "34", cy: "34", r: "10", fill: "#0F8B8D" }
                            circle { cx: "94", cy: "34", r: "10", fill: "#E85D04" }
                            circle { cx: "64", cy: "94", r: "10", fill: "#F4D35E" }
                            path { d: "M34 34L94 34L64 94L34 34Z", stroke: "#F7F3EC", stroke_width: "8", stroke_linejoin: "round" }
                        }
                        span { class: "font-mono text-[14px] text-fg", "shardd" }
                    }
                    h1 { class: "text-[32px] font-mono font-normal leading-[100%] tracking-[-0.04em] text-fg", "Privacy Policy" }
                    p { class: "font-mono text-[14px] text-base-500 leading-[160%]",
                        "Minimal version without tracking or ads. The service is operated by TQDM Inc. in the USA."
                    }
                    div { class: "grid gap-4 font-mono text-[14px] text-base-400 leading-[160%]",

                        {section("Controller", &[
                            "TQDM Inc., 1111B S Governors Ave, STE 23256, Dover, DE 19904, USA",
                            "Email: contact@tqdm.org | Phone: +1 (814) 524-5685",
                        ])}

                        {section("Data processed", &[
                            "Your email address (for authentication and notifications)",
                            "API keys and their scopes (created by you)",
                            "Bucket names, event data, and account balances (created by you via the API)",
                            "Audit logs of administrative actions",
                            "Technical server logs (IP address, timestamp, error messages)",
                            "Google profile email when using Google Sign-In",
                            "No tracking cookies, no analytics, no advertising data",
                        ])}

                        {section("Purpose and legal basis", &[
                            "Provide the shardd control plane and API service",
                            "Authenticate users and manage sessions",
                            "Maintain technical functionality, security, and audit trails",
                            "Legal basis: Art. 6(1)(b) GDPR (providing the service); Art. 6(1)(f) GDPR for logs (legitimate interest in operation/security)",
                        ])}

                        {section("Storage and deletion", &[
                            "Account data is stored while you use the service.",
                            "Deleting your account removes your user record, API keys, and association with bucket data.",
                            "Event data written to the distributed ledger is replicated across mesh nodes and may persist in replicas after account deletion.",
                            "Server logs are automatically deleted after a short retention period.",
                        ])}

                        {section("Sharing of data", &[
                            "No disclosure to third parties except technical providers (cloud hosting, email delivery) required to operate the service.",
                            "Bucket data is distributed across mesh nodes in multiple geographic regions as part of the service's architecture.",
                            "No advertising, no tracking, no sale of data.",
                        ])}

                        {section("Transfers to the USA", &[
                            "As a US company, data processing occurs in the USA and other regions where mesh nodes are deployed.",
                            "For EU users: based on EU Standard Contractual Clauses (SCCs) plus supplementary technical and organizational measures.",
                        ])}

                        {section("Your rights", &[
                            "Access, rectification, erasure, restriction, portability (Art. 15–20 GDPR)",
                            "Objection to certain processing (Art. 21 GDPR)",
                            "Withdraw consent with future effect",
                            "Right to lodge a complaint with a data protection authority",
                            "Contact to exercise rights: contact@tqdm.org",
                        ])}

                        {section("Cookies", &[
                            "We use HTTP-only session cookies for authentication (access_token, refresh_token, login_session).",
                            "No tracking cookies, no third-party cookies, no analytics scripts.",
                        ])}

                        {section("Security", &[
                            "Appropriate technical and organizational measures protect against loss, misuse, and unauthorized access.",
                            "All API traffic is encrypted via TLS. Authentication tokens are HTTP-only and cannot be accessed by client-side scripts.",
                            "Please secure your email inbox and devices since login links and Google OAuth grants provide account access.",
                        ])}

                        {section("Changes", &[
                            "This privacy notice may be updated as needed. The current version is always published here.",
                        ])}

                        {section("Contact", &[
                            "TQDM Inc., 1111B S Governors Ave, STE 23256, Dover, DE 19904, USA",
                            "Email: contact@tqdm.org | Phone: +1 (814) 524-5685",
                        ])}

                        p { class: "text-base-600 mt-4", "Last updated: April 2026" }
                    }
                }
            }
            {legal_footer()}
        }
    }
}
