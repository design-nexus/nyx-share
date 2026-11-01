//! Turning a caller's item list and descriptors into payloads the providers can read.

use crate::caller::Caller;
use nyx_share_core::{ContentKind, ItemInfo, ItemSpec, Limits, mime, names, validate};
use nyx_share_providers::{Data, Payload};
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use zbus::fdo;

/// The most files one share may hold once its folders are opened up.
const MAX_FILES: usize = 1000;
/// How deep a shared folder is followed.
const MAX_DEPTH: usize = 16;

pub fn parse_specs(json: &str) -> fdo::Result<Vec<ItemSpec>> {
    serde_json::from_str(json).map_err(|e| fdo::Error::InvalidArgs(format!("The items aren't valid JSON: {e}")))
}

pub fn build(specs: Vec<ItemSpec>, fds: Vec<zbus::zvariant::OwnedFd>, caller: &Caller) -> fdo::Result<Vec<Payload>> {
    validate(&specs, fds.len(), &Limits::default()).map_err(|e| fdo::Error::InvalidArgs(e.to_string()))?;
    let mut fds: Vec<Option<OwnedFd>> = fds.into_iter().map(|fd| Some(OwnedFd::from(fd))).collect();
    let mut payloads = Vec::new();
    for (i, spec) in specs.into_iter().enumerate() {
        let payload = (|| {
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
                    folder: None,
                };
                return Ok(Some(Payload { info, data: Data::Text(text) }));
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
            check_readable(&file).map_err(|e| fdo::Error::InvalidArgs(format!("Item {n} {e}")))?;
            let meta = file.metadata().map_err(|e| fdo::Error::InvalidArgs(format!("Item {n} can't be read: {e}")))?;
            if meta.is_dir() {
                let name = spec.name.or(path_name).or_else(|| dir_name(&file)).unwrap_or_else(|| "Shared folder".into());
                let top = names::sanitize(&name);
                let before = payloads.len();
                walk(file, &top, 0, &mut payloads).map_err(|e| fdo::Error::InvalidArgs(format!("Item {n}: {e}")))?;
                if payloads.len() == before {
                    return Err(fdo::Error::InvalidArgs(format!(
                        "{top} has no files to share (hidden files and links are left out)"
                    )));
                }
                return Ok(None);
            }
            if !meta.is_file() {
                return Err(fdo::Error::InvalidArgs(format!("Item {n} isn't a regular file")));
            }
            let size = meta.len();
            let raw_name = spec.name.or(path_name).unwrap_or_else(|| names::FALLBACK.into());
            let name = names::sanitize(&raw_name);
            let mime = spec.mime.or_else(|| mime::guess(&name).map(str::to_string));
            if spec.kind == ContentKind::Image && mime.as_deref().is_some_and(|m| !m.starts_with("image/")) {
                return Err(fdo::Error::InvalidArgs(format!("Item {n} is marked as an image but is {}", mime.unwrap())));
            }
            Ok(Some(Payload {
                info: ItemInfo { kind: spec.kind, name, mime, size, preview: None, folder: None },
                data: Data::File(file),
            }))
        })()?;
        payloads.extend(payload);
        if payloads.len() > MAX_FILES {
            return Err(fdo::Error::InvalidArgs(format!("Share at most {MAX_FILES} files at a time, folders included")));
        }
    }
    Ok(payloads)
}

/// A folder descriptor's own name, from where it is open.
fn dir_name(dir: &File) -> Option<String> {
    let path = std::fs::read_link(format!("/proc/self/fd/{}", dir.as_raw_fd())).ok()?;
    path.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// Add a folder's files, sorted, with `folder` as where they sit. Each entry is
/// opened relative to its folder's descriptor without following links, so a
/// link inside can't reach anything the caller couldn't. Hidden entries, links
/// and special files are left out.
fn walk(dir: File, folder: &str, depth: usize, out: &mut Vec<Payload>) -> Result<(), String> {
    if depth >= MAX_DEPTH {
        return Err(format!("{folder} goes more than {MAX_DEPTH} folders deep"));
    }
    let listing =
        std::fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd())).map_err(|e| format!("{folder} can't be listed: {e}"))?;
    let mut entries: Vec<(String, std::fs::FileType)> = listing
        .filter_map(Result::ok)
        .filter_map(|e| Some((e.file_name().into_string().ok()?, e.file_type().ok()?)))
        .filter(|(name, kind)| !name.starts_with('.') && (kind.is_file() || kind.is_dir()))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, kind) in entries {
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | libc::O_NONBLOCK
            | if kind.is_dir() { libc::O_DIRECTORY } else { 0 };
        let Some(child) = open_at(&dir, &name, flags) else { continue };
        let Ok(meta) = child.metadata() else { continue };
        if meta.is_dir() {
            walk(child, &format!("{folder}/{}", names::sanitize(&name)), depth + 1, out)?;
        } else if meta.is_file() {
            let mime = mime::guess(&name).map(str::to_string);
            let kind =
                if mime.as_deref().is_some_and(|m| m.starts_with("image/")) { ContentKind::Image } else { ContentKind::File };
            let info = ItemInfo {
                kind,
                name: names::sanitize(&name),
                mime,
                size: meta.len(),
                preview: None,
                folder: Some(folder.to_string()),
            };
            out.push(Payload { info, data: Data::File(child) });
        }
        if out.len() > MAX_FILES {
            return Err(format!("Share at most {MAX_FILES} files at a time, folders included"));
        }
    }
    Ok(())
}

fn open_at(dir: &File, name: &str, flags: i32) -> Option<File> {
    let c = std::ffi::CString::new(name).ok()?;
    // SAFETY: `dir` is an open descriptor and `c` a valid C string.
    let fd = unsafe { libc::openat(dir.as_raw_fd(), c.as_ptr(), flags) };
    // SAFETY: a non-negative result is a new descriptor we own.
    (fd >= 0).then(|| unsafe { File::from_raw_fd(fd) })
}

/// A shared descriptor must be opened read-only, not as O_PATH.
fn check_readable(file: &File) -> Result<(), String> {
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
    Ok(())
}
