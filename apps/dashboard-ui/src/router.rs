use crate::pages::account_detail::AccountDetail;
use crate::pages::admin_audit::AdminAudit;
use crate::pages::admin_events::AdminEvents;
use crate::pages::admin_mesh::AdminMesh;
use crate::pages::admin_overview::AdminOverview;
use crate::pages::admin_user::AdminUser;
use crate::pages::admin_users::AdminUsers;
use crate::pages::billing::Billing;
use crate::pages::bucket_detail::BucketDetail;
use crate::pages::buckets::Buckets;
use crate::pages::contact::Contact;
use crate::pages::dashboard::Dashboard;
use crate::pages::developer_events::Events;
use crate::pages::keys::Keys;
use crate::pages::legal::{Privacy, Tos};
use crate::pages::login::{Login, Magic};
use crate::pages::not_found::NotFound;
use crate::pages::profile::Profile;
use dioxus::prelude::*;

#[derive(Clone, Routable, Debug, PartialEq)]
pub enum Route {
    #[route("/login")]
    Login,
    #[route("/magic")]
    Magic,
    #[route("/tos")]
    Tos,
    #[route("/privacy")]
    Privacy,
    #[layout(Shell)]
    #[route("/dashboard")]
    Dashboard,
    #[route("/dashboard/keys")]
    Keys,
    #[route("/dashboard/billing")]
    Billing,
    #[route("/dashboard/events")]
    Events,
    #[route("/dashboard/buckets")]
    Buckets,
    #[route("/dashboard/buckets/:bucket")]
    BucketDetail { bucket: String },
    #[route("/dashboard/buckets/:bucket/accounts/:account")]
    AccountDetail { bucket: String, account: String },
    #[route("/profile")]
    Profile,
    #[route("/dashboard/contact")]
    Contact,
    #[route("/admin")]
    AdminOverview,
    #[route("/admin/users")]
    AdminUsers,
    #[route("/admin/users/:user_id")]
    AdminUser { user_id: String },
    #[route("/admin/events")]
    AdminEvents,
    #[route("/admin/audit")]
    AdminAudit,
    #[route("/admin/mesh")]
    AdminMesh,
    #[end_layout]
    #[route("/:..segments")]
    NotFound { segments: Vec<String> },
}

#[component]
fn Shell() -> Element {
    rsx! {
        div { class: "min-h-screen",
            crate::pages::shell::ShellLayout {}
        }
    }
}
