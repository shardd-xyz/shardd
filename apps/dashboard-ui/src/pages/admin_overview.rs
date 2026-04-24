use crate::api;
use crate::components::mesh_graph::MeshTopologyGraph;
use crate::components::stat_card::StatCard;
use crate::router::Route;
use dioxus::prelude::*;

#[component]
pub fn AdminOverview() -> Element {
    let data = use_resource(|| async {
        let (stats, edges) = futures_util::join!(api::admin::stats(), api::buckets::list_edges(),);
        (stats.ok(), edges.unwrap_or_default())
    });

    rsx! {
        div { class: "grid gap-6 w-full",
        section { class: "flex flex-wrap justify-between items-start gap-4",
            div { class: "grid gap-1",
                span { class: "text-accent-100 text-xs uppercase tracking-widest", "Operations" }
                h1 { class: "text-[32px] font-normal leading-[100%] tracking-[-0.04em] font-mono tracking-tight", "Overview" }
            }
            div { class: "flex gap-2.5",
                Link { to: Route::AdminUsers, class: "bg-[var(--btn-primary-bg)] hover:bg-base-800 text-[var(--btn-primary-text)] font-normal px-4 py-2 rounded-lg transition no-underline text-sm", "Open users" }
                Link { to: Route::AdminMesh, class: "px-3.5 py-2 rounded-full border border-base-700 text-base-400 hover:text-fg transition no-underline text-sm", "Mesh" }
                Link { to: Route::AdminAudit, class: "px-3.5 py-2 rounded-full border border-base-700 text-base-400 hover:text-fg transition no-underline text-sm", "Open audit" }
            }
        }

        match &*data.read() {
            Some((Some(stats), edges)) => rsx! {
                if stats.frozen_users > 0 {
                    {
                        let label = if stats.frozen_users == 1 { "account" } else { "accounts" };
                        rsx! {
                            section { class: "flex flex-wrap items-baseline gap-2 px-4 py-3 rounded-sm border border-accent-200/30 bg-base-900",
                                strong { class: "text-fg", "{stats.frozen_users} frozen {label}." }
                                Link { to: Route::AdminUsers, class: "text-accent-100 hover:text-fg no-underline text-sm", "Review users" }
                            }
                        }
                    }
                }

                section { class: "grid grid-cols-[repeat(auto-fit,minmax(160px,1fr))] gap-4",
                    StatCard { label: "Total users".to_string(), value: stats.total_users.to_string() }
                    StatCard { label: "New in 7 days".to_string(), value: stats.users_last_7_days.to_string() }
                    StatCard { label: "Frozen".to_string(), value: stats.frozen_users.to_string() }
                    StatCard { label: "Admins".to_string(), value: stats.admin_users.to_string() }
                }

                section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-5",
                    h2 { class: "text-[16px] font-normal", "Quick links" }
                    div { class: "grid grid-cols-2 gap-4",
                        Link { to: Route::AdminUsers, class: "rounded-lg border border-base-800 bg-base-900 p-4 grid gap-1 hover:border-base-700 transition no-underline",
                            span { class: "text-accent-100 text-xs uppercase tracking-widest", "Users" }
                            strong { class: "text-fg", "Inspect accounts" }
                            span { class: "text-base-500 text-sm", "Search users, open detail views, and impersonate when support work needs the exact user flow." }
                        }
                        Link { to: Route::AdminAudit, class: "rounded-lg border border-base-800 bg-base-900 p-4 grid gap-1 hover:border-base-700 transition no-underline",
                            span { class: "text-accent-100 text-xs uppercase tracking-widest", "Audit" }
                            strong { class: "text-fg", "Review admin activity" }
                            span { class: "text-base-500 text-sm", "Check privileged actions without exposing raw metadata until you explicitly open it." }
                        }
                    }
                }

                if !edges.is_empty() {
                    MeshTopologyGraph { edges: edges.clone() }
                }
            },
            _ => rsx! { div { class: "text-base-500 text-center py-12", "Loading…" } },
        }
        }
    }
}
