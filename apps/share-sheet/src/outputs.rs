//! What a job made: a local path, or a URI on a NAS (`smb://…`).

use gtk::prelude::*;
use gtk::{gio, glib};
use std::path::Path;

fn is_uri(output: &str) -> bool {
    output.split_once("://").is_some_and(|(scheme, _)| !scheme.is_empty() && !scheme.contains('/'))
}

/// For people: `~/Downloads/a.pdf`, or `nas.local/Documents/Inbox/a.pdf`.
pub fn pretty(output: &str) -> String {
    if !is_uri(output) {
        return nyx_core::paths::pretty(Path::new(output));
    }
    let rest = output.split_once("://").map_or(output, |(_, rest)| rest);
    glib::uri_unescape_string(rest, None::<&str>).map_or_else(|| rest.to_string(), |s| s.to_string())
}

/// Show the output selected in the file manager. URIs go as they are, so a
/// file on a NAS opens over the same GVfs connection.
pub fn show(output: &str) {
    if !is_uri(output) {
        nyx_core::cmd::show_in_folder(Path::new(output));
        return;
    }
    let uri = output.to_string();
    let parent = gio::File::for_uri(output).parent().map(|p| p.uri().to_string()).unwrap_or_else(|| uri.clone());
    gio::bus_get(gio::BusType::Session, gio::Cancellable::NONE, move |bus| {
        let open_parent = move || {
            let _ = gio::AppInfo::launch_default_for_uri(&parent, gio::AppLaunchContext::NONE);
        };
        let Ok(bus) = bus else { return open_parent() };
        bus.call(
            Some("org.freedesktop.FileManager1"),
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1",
            "ShowItems",
            Some(&(vec![uri], String::new()).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            2000,
            gio::Cancellable::NONE,
            move |res: Result<glib::Variant, glib::Error>| {
                if res.is_err() {
                    open_parent();
                }
            },
        );
    });
}

/// Fill `bar` with the job's progress, or let it breathe while the job connects.
pub fn set_progress(bar: &gtk::LevelBar, job: &nyx_share_client::Job) {
    if job.step.is_some() && !job.state.finished() {
        bar.set_value(1.0);
        bar.add_css_class("connecting");
    } else {
        bar.remove_css_class("connecting");
        bar.set_value(job.fraction().unwrap_or(0.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_nas_outputs_without_the_scheme_or_escapes() {
        assert_eq!(pretty("smb://nas.local/Documents/Inbox/Q3%20report.pdf"), "nas.local/Documents/Inbox/Q3 report.pdf");
        assert!(is_uri("sftp://ken@host:2222/srv/a"));
        assert!(!is_uri("/home/ken/a://b"));
    }
}
