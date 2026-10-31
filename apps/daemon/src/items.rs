//! Turning a caller's item list and descriptors into payloads the providers can read.

use crate::caller::Caller;
use nyx_share_core::{ContentKind, ItemInfo, ItemSpec, Limits, mime, names, validate};
use nyx_share_providers::{Data, Payload};
use std::fs::File;
use std::os::fd::{AsRawFd, OwnedFd};
use zbus::fdo;

pub fn parse_specs(json: &str) -> fdo::Result<Vec<ItemSpec>> {
    serde_json::from_str(json).map_err(|e| fdo::Error::InvalidArgs(format!("The items aren't valid JSON: {e}")))
}

pub fn build(specs: Vec<ItemSpec>, fds: Vec<zbus::zvariant::OwnedFd>, caller: &Caller) -> fdo::Result<Vec<Payload>> {
    validate(&specs, fds.len(), &Limits::default()).map_err(|e| fdo::Error::InvalidArgs(e.to_string()))?;
    let mut fds: Vec<Option<OwnedFd>> = fds.into_iter().map(|fd| Some(OwnedFd::from(fd))).collect();
    specs
        .into_iter()
        .enumerate()
        .map(|(i, spec)| {
            let n = i + 1;
            if spec.kind.is_inline() {
                let text = spec.text.unwrap_or_default();
                let text = if spec.kind == ContentKind::Url { text.trim().to_string() } else { text };
                let fallback = if spec.kind == ContentKind::Url { "Link" } else { "Shared text" };
                let info = ItemInfo {
                    kind: spec.kind,
                    name: spec.name.as_deref().map(names::sanitize).unwrap_or_else(|| fallback.into()),
                    mime: None,
                    size: text.len() as u64,
                    preview: Some(text.chars().take(500).collect()),
                };
                return Ok(Payload { info, data: Data::Text(text) });
            }
            let (file, path_name) = match (spec.fd, spec.path.as_deref()) {
                (Some(index), _) => (File::from(fds[index as usize].take().expect("validated")), None),
                (None, Some(_)) if caller.sandboxed => {
                    return Err(fdo::Error::AccessDenied(format!(
                        "Item {n}: sandboxed apps must send file descriptors, not paths"
                    )));
                }
                (None, Some(path)) => {
                    let file = File::open(path).map_err(|e| fdo::Error::InvalidArgs(format!("Item {n}: {path}: {e}")))?;
                    let name = std::path::Path::new(path).file_name().map(|s| s.to_string_lossy().into_owned());
                    (file, name)
                }
                (None, None) => unreachable!("validated"),
            };
            let size = check_file(&file).map_err(|e| fdo::Error::InvalidArgs(format!("Item {n} {e}")))?;
            let raw_name = spec.name.or(path_name).unwrap_or_else(|| names::FALLBACK.into());
            let name = names::sanitize(&raw_name);
            let mime = spec.mime.or_else(|| mime::guess(&name).map(str::to_string));
            if spec.kind == ContentKind::Image && mime.as_deref().is_some_and(|m| !m.starts_with("image/")) {
                return Err(fdo::Error::InvalidArgs(format!("Item {n} is marked as an image but is {}", mime.unwrap())));
            }
            Ok(Payload { info: ItemInfo { kind: spec.kind, name, mime, size, preview: None }, data: Data::File(file) })
        })
        .collect()
}

/// A shared descriptor must be a regular file opened read-only. Returns its size.
fn check_file(file: &File) -> Result<u64, String> {
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is open for the lifetime of `file`.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err("has a descriptor that can't be checked".into());
    }
    if flags & libc::O_PATH != 0 {
        return Err("was opened with O_PATH and can't be read".into());
    }
    if flags & libc::O_ACCMODE != libc::O_RDONLY {
        return Err("must be opened read-only".into());
    }
    let meta = file.metadata().map_err(|e| format!("can't be read: {e}"))?;
    if !meta.is_file() {
        return Err(if meta.is_dir() {
            "is a folder. Folders can't be shared yet.".into()
        } else {
            "isn't a regular file".into()
        });
    }
    Ok(meta.len())
}
