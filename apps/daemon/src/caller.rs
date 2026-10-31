//! Who is calling: the same user, and whether the caller is sandboxed.

use zbus::fdo;
use zbus::message::Header;

pub struct Caller {
    pub sandboxed: bool,
}

pub async fn identify(conn: &zbus::Connection, header: &Header<'_>) -> fdo::Result<Caller> {
    let sender = header.sender().ok_or_else(|| fdo::Error::AccessDenied("The caller has no bus name".into()))?;
    let dbus = fdo::DBusProxy::new(conn).await.map_err(|e| fdo::Error::Failed(e.to_string()))?;
    let creds = dbus.get_connection_credentials(sender.clone().into()).await?;
    // SAFETY: getuid has no failure modes.
    let me = unsafe { libc::getuid() };
    if creds.unix_user_id().is_some_and(|uid| uid != me) {
        return Err(fdo::Error::AccessDenied("Only this user's apps can share".into()));
    }
    // Without a process id the caller can't be checked, so it is treated as sandboxed.
    let sandboxed = creds.process_id().is_none_or(is_sandboxed);
    Ok(Caller { sandboxed })
}

/// Flatpak and Snap apps: a `.flatpak-info` in their root, or their cgroup.
fn is_sandboxed(pid: u32) -> bool {
    if std::path::Path::new(&format!("/proc/{pid}/root/.flatpak-info")).exists() {
        return true;
    }
    match std::fs::read_to_string(format!("/proc/{pid}/cgroup")) {
        Ok(cgroup) => cgroup.contains("/app-flatpak-") || cgroup.contains("/snap."),
        Err(_) => true,
    }
}
