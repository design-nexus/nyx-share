//! nyx-share-sheet: the Share Sheet. The Share service opens it for a request;
//! apps don't run it directly (they call `Share` on the service).

mod prefs;
mod sheet;
mod theme;
mod tile;

use gtk::prelude::*;
use gtk::{gio, glib};
use nyx_share_dbus::{ENV_REQUEST, ENV_TOKEN};

pub const APP_ID: &str = "io.github.design_nexus.nyx.Share";

fn main() -> glib::ExitCode {
    nyx_core::app::init(nyx_core::app::App { slug: "nyx-share", name: "Share" });
    if std::env::var_os("GSK_RENDERER").is_none() {
        // SAFETY: single-threaded startup before GTK initializes
        unsafe { std::env::set_var("GSK_RENDERER", "ngl") };
    }
    let id = std::env::var("NYX_SHARE_APP_ID").unwrap_or_else(|_| APP_ID.to_string());
    let app = gtk::Application::builder().application_id(id).flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE).build();
    app.connect_startup(|_| theme::install());
    app.connect_command_line(|app, cl| {
        let var = |name: &str| cl.getenv(name).map(|v| v.to_string()).filter(|v| !v.is_empty());
        match (var(ENV_REQUEST), var(ENV_TOKEN)) {
            (Some(request), Some(token)) => {
                sheet::open(app, request, token);
                glib::ExitCode::SUCCESS
            }
            _ => {
                eprintln!("The Share Sheet is opened by the Share service. To share, run: nyx-share FILE");
                glib::ExitCode::from(2)
            }
        }
    });
    app.connect_shutdown(|_| prefs::flush());
    app.run()
}
