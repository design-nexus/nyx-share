use std::collections::HashMap;
use zbus::zvariant::Value;

/// A desktop notification. Failures are only logged: a missing notification
/// daemon mustn't fail a transfer.
pub fn send(conn: &zbus::Connection, summary: &str, body: &str) {
    let hints: HashMap<&str, Value<'_>> = HashMap::from([("desktop-entry", Value::from("io.github.design_nexus.nyx.Share"))]);
    let result = async_io::block_on(conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &("Share", 0u32, "io.github.design_nexus.nyx.Share", summary, body, Vec::<&str>::new(), hints, 5000i32),
    ));
    if let Err(e) = result {
        eprintln!("nyx-share: couldn't notify: {e}");
    }
}
