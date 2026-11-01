//! Cloud accounts from Nyx Cloud (`io.github.design_nexus.nyx.Cloud1`): files
//! are copied into the account's mounted folder, which rclone uploads. Share
//! asks Cloud to mount the account if it isn't, and never deletes anything.

use nyx_share_core::{ContentKind, Destination, Section, names};
use nyx_share_providers::{Data, Outcome, Provider, SendError, Task};
use serde::Deserialize;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const PREFIX: &str = "cloud:";
const PATH: &str = "/io/github/design_nexus/nyx/Cloud1";
const INTERFACE: &str = "io.github.design_nexus.nyx.Cloud1";

#[derive(Clone, Debug, Deserialize)]
pub struct Remote {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub connection: String,
    #[serde(default)]
    pub mount_status: String,
    #[serde(default)]
    pub mount_path: String,
    #[serde(default)]
    pub error: String,
    #[serde(default = "yes")]
    pub storage_enabled: bool,
}

fn yes() -> bool {
    true
}

pub struct Cloud {
    /// `NYX_SHARE_CLOUD_BUS` points checks at a stand-in service.
    bus_name: String,
}

impl Default for Cloud {
    fn default() -> Self {
        let bus_name = std::env::var("NYX_SHARE_CLOUD_BUS").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| INTERFACE.into());
        Self { bus_name }
    }
}

impl Cloud {
    fn call(&self, bus: &zbus::blocking::Connection, method: &str, arg: Option<&str>) -> Result<String, String> {
        let reply = match arg {
            Some(a) => bus.call_method(Some(self.bus_name.as_str()), PATH, Some(INTERFACE), method, &(a,)),
            None => bus.call_method(Some(self.bus_name.as_str()), PATH, Some(INTERFACE), method, &()),
        }
        .map_err(|e| match e {
            zbus::Error::MethodError(_, Some(text), _) => text,
            other => format!("Cloud didn't answer: {other}"),
        })?;
        reply.body().deserialize::<String>().map_err(|e| format!("Cloud answered oddly: {e}"))
    }

    fn remotes(&self, bus: &zbus::blocking::Connection) -> Result<Vec<Remote>, String> {
        let json = self.call(bus, "ListRemotes", None)?;
        serde_json::from_str(&json).map_err(|e| format!("Cloud answered oddly: {e}"))
    }

    /// The account's mounted folder, mounting it first if needed.
    fn mounted(&self, bus: &zbus::blocking::Connection, id: &str, task: &Task<'_>) -> Result<(Remote, PathBuf), SendError> {
        let find = || -> Result<Remote, SendError> {
            self.remotes(bus)
                .map_err(SendError::failed)?
                .into_iter()
                .find(|r| r.id == id)
                .ok_or_else(|| SendError::failed("That cloud account isn't in Cloud any more"))
        };
        let mut remote = find()?;
        if remote.mount_status != "mounted" {
            (task.step)(Some(&format!("Connecting to {}…", remote.name)));
            self.call(bus, "Mount", Some(id)).map_err(SendError::failed)?;
            let start = Instant::now();
            loop {
                if task.cancelled() {
                    return Err(SendError::Cancelled { outputs: Vec::new() });
                }
                std::thread::sleep(Duration::from_millis(500));
                remote = find()?;
                match remote.mount_status.as_str() {
                    "mounted" => break,
                    "error" => return Err(SendError::failed(format!("{} couldn't connect: {}", remote.name, remote.error))),
                    _ if start.elapsed() > Duration::from_secs(90) => {
                        return Err(SendError::failed(format!("{} didn't connect within 90 seconds", remote.name)));
                    }
                    _ => {}
                }
            }
            (task.step)(None);
        }
        let path = PathBuf::from(&remote.mount_path);
        if !path.is_absolute() || !path.is_dir() {
            return Err(SendError::failed(format!("{}'s folder isn't there", remote.name)));
        }
        Ok((remote, path))
    }
}

/// Reads an open file from the start with positional reads: its offset may be
/// shared with the Share Sheet's preview.
struct At<'a> {
    file: &'a std::fs::File,
    offset: u64,
}

impl std::io::Read for At<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.file.read_at(buf, self.offset)?;
        self.offset += n as u64;
        Ok(n)
    }
}

/// A subfolder for this send (`Reports/2026`), made if missing. No `.` or `..`.
fn subfolder(base: &Path, sub: Option<&str>) -> Result<PathBuf, SendError> {
    let Some(sub) = sub.filter(|s| !s.trim().is_empty()) else { return Ok(base.to_path_buf()) };
    let mut dir = base.to_path_buf();
    for part in sub.split('/').map(str::trim).filter(|p| !p.is_empty()) {
        if part == "." || part == ".." {
            return Err(SendError::failed("The folder can't contain . or .."));
        }
        dir.push(names::sanitize(part));
    }
    std::fs::create_dir_all(&dir).map_err(|e| SendError::failed(format!("Couldn't make the folder: {e}")))?;
    Ok(dir)
}

impl Provider for Cloud {
    fn id(&self) -> &'static str {
        "cloud"
    }

    fn name(&self) -> &'static str {
        "Cloud"
    }

    fn kinds(&self) -> Vec<ContentKind> {
        vec![ContentKind::Text, ContentKind::Image, ContentKind::File]
    }

    fn destinations(&self) -> Vec<Destination> {
        let Ok(bus) = zbus::blocking::Connection::session() else { return Vec::new() };
        // Listing destinations doesn't start Cloud; while it isn't running there's nothing to offer.
        let running = zbus::blocking::fdo::DBusProxy::new(&bus)
            .ok()
            .and_then(|p| p.name_has_owner(self.bus_name.as_str().try_into().ok()?).ok())
            .unwrap_or(false);
        if !running {
            return Vec::new();
        }
        let Ok(remotes) = self.remotes(&bus) else { return Vec::new() };
        remotes
            .into_iter()
            .filter(|r| r.storage_enabled)
            .map(|r| Destination {
                id: format!("{PREFIX}{}", r.id),
                provider: self.id().into(),
                name: r.name.clone(),
                icon: "folder-remote-symbolic".into(),
                tint: "blue".into(),
                section: Section::Apps,
                kinds: self.kinds(),
                max_items: None,
                needs_folder: false,
                available: r.connection != "error",
                status: Some(match r.mount_status.as_str() {
                    "mounted" => "Cloud".into(),
                    _ if !r.error.is_empty() => r.error.clone(),
                    _ => "Cloud · connects when you send".into(),
                }),
                pinned: false,
                hidden: false,
            })
            .collect()
    }

    fn send(&self, destination: &str, task: &Task<'_>) -> Result<Outcome, SendError> {
        let id = destination.strip_prefix(PREFIX).unwrap_or(destination);
        let bus = zbus::blocking::Connection::session().map_err(|e| SendError::failed(format!("No session bus: {e}")))?;
        let (remote, base) = self.mounted(&bus, id, task)?;
        let dir = subfolder(&base, task.options.subfolder.as_deref())?;
        let mut outputs = Vec::new();
        for item in task.items {
            if task.cancelled() {
                return Err(SendError::Cancelled { outputs });
            }
            let saved = match &item.data {
                Data::Text(text) => {
                    let name = names::for_text(Some(&item.info.name));
                    nyx_share_providers::save_stream(
                        &dir,
                        &name,
                        &mut text.as_bytes(),
                        text.len() as u64,
                        task.cancel,
                        task.progress,
                    )
                }
                Data::File(file) => {
                    let name = names::sanitize(&item.info.name);
                    let mut reader = At { file, offset: 0 };
                    nyx_share_providers::save_stream(&dir, &name, &mut reader, item.info.size, task.cancel, task.progress)
                }
            };
            match saved {
                Ok(path) => outputs.push(path.display().to_string()),
                Err(SendError::Cancelled { .. }) => return Err(SendError::Cancelled { outputs }),
                Err(SendError::Failed { message, .. }) => return Err(SendError::Failed { message, outputs }),
            }
        }
        let (what, it) = match outputs.len() {
            1 => (Path::new(&outputs[0]).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), "it"),
            n => (format!("{n} items"), "them"),
        };
        Ok(Outcome { message: format!("Copied {what} to {}; Cloud uploads {it} in the background", remote.name), outputs })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subfolders_stay_inside() {
        let base = std::env::temp_dir().join(format!("nyx-share-cloud-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        assert_eq!(subfolder(&base, Some(" Reports / 2026 ")).unwrap(), base.join("Reports/2026"));
        assert!(subfolder(&base, Some("a/../b")).is_err());
        assert_eq!(subfolder(&base, None).unwrap(), base);
        std::fs::remove_dir_all(base).unwrap();
    }
}
