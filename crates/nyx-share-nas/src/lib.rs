//! NAS destinations: a folder on a file server, reached through GVfs (the
//! storage layer Nyx Files uses), so SMB, SFTP and NFS work without anything
//! running on the NAS. Files are copied, never moved, and never replace
//! anything there. Passwords live in the Secret Service, not in settings.

pub mod secrets;

use gio::prelude::*;
use nyx_share_core::{ContentKind, Destination, Section, names};
use nyx_share_providers::{Data, Outcome, Provider, SendError, Task};
use serde::{Deserialize, Serialize};
use std::os::unix::fs::FileExt as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

pub const PREFIX: &str = "nas:";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Conflict {
    /// Save as "name (2).ext" when the name is taken.
    #[default]
    KeepBoth,
    /// Leave the file on the NAS alone and don't copy that item.
    Skip,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Nas {
    pub id: String,
    pub name: String,
    /// The folder: `smb://host/share/path`, `sftp://user@host/path`,
    /// `nfs://host/export/path`, or `file:///mnt/nas/path` for a local mount.
    pub uri: String,
    /// The same folder over Tailscale, tried when the first fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tailscale_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default)]
    pub conflict: Conflict,
}

impl Nas {
    pub fn protocol(&self) -> &'static str {
        match self.uri.split_once("://").map(|(s, _)| s.to_ascii_lowercase()).as_deref() {
            Some("smb") => "SMB",
            Some("sftp") | Some("ssh") => "SFTP",
            Some("nfs") => "NFS",
            Some("file") => "Folder",
            _ => "Network",
        }
    }

    pub fn host(&self) -> String {
        let rest = self.uri.split_once("://").map(|(_, r)| r).unwrap_or(&self.uri);
        let host = rest.split('/').next().unwrap_or_default();
        host.rsplit('@').next().unwrap_or(host).to_string()
    }

    /// Check it's a folder address GVfs can reach.
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Give the NAS folder a name".into());
        }
        for uri in std::iter::once(&self.uri).chain(self.tailscale_uri.iter()) {
            let scheme = uri
                .split_once("://")
                .map(|(s, _)| s.to_ascii_lowercase())
                .ok_or("The address needs smb://, sftp://, nfs:// or file://")?;
            if !matches!(scheme.as_str(), "smb" | "sftp" | "ssh" | "nfs" | "file") {
                return Err(format!("{scheme}:// isn't supported; use smb://, sftp://, nfs:// or file://"));
            }
            if uri.contains("/../") || uri.ends_with("/..") {
                return Err("The address can't contain ..".into());
            }
        }
        Ok(())
    }
}

/// A relative folder under the NAS folder: each part a safe name, no `..`.
pub fn clean_subfolder(sub: &str) -> Result<Vec<String>, String> {
    let parts: Vec<String> = sub.split('/').map(str::trim).filter(|p| !p.is_empty()).map(String::from).collect();
    if parts.iter().any(|p| p == ".." || p == ".") {
        return Err("The folder can't contain . or ..".into());
    }
    Ok(parts.iter().map(|p| names::sanitize(p)).collect())
}

pub struct NasProvider(pub Arc<RwLock<Vec<Nas>>>);

impl NasProvider {
    fn list(&self) -> Vec<Nas> {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Provider for NasProvider {
    fn id(&self) -> &'static str {
        "nas"
    }

    fn name(&self) -> &'static str {
        "NAS"
    }

    fn kinds(&self) -> Vec<ContentKind> {
        vec![ContentKind::Text, ContentKind::Image, ContentKind::File]
    }

    fn destinations(&self) -> Vec<Destination> {
        self.list()
            .into_iter()
            .map(|n| Destination {
                id: format!("{PREFIX}{}", n.id),
                provider: self.id().into(),
                name: n.name.clone(),
                icon: "network-server-symbolic".into(),
                tint: "amber".into(),
                section: Section::Devices,
                kinds: self.kinds(),
                max_items: None,
                needs_folder: false,
                available: true,
                status: Some(match n.host() {
                    h if h.is_empty() => n.protocol().to_string(),
                    h => format!("{} · {h}", n.protocol()),
                }),
                pinned: false,
                hidden: false,
                default: false,
            })
            .collect()
    }

    fn send(&self, destination: &str, task: &Task<'_>) -> Result<Outcome, SendError> {
        let id = destination.strip_prefix(PREFIX).unwrap_or(destination);
        let nas = self
            .list()
            .into_iter()
            .find(|n| n.id == id)
            .ok_or_else(|| SendError::failed("That NAS folder isn't saved any more"))?;
        let sub = clean_subfolder(task.options.subfolder.as_deref().unwrap_or_default()).map_err(SendError::failed)?;
        copy(&nas, &sub, task)
    }
}

/// Run GVfs work that needs a main loop (mounting) on this worker thread.
/// How long one attempt to mount may take; slow SMB servers need about a minute.
const MOUNT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

fn with_context<T>(f: impl FnOnce(&glib::MainContext) -> T) -> T {
    let ctx = glib::MainContext::new();
    ctx.with_thread_default(|| f(&ctx)).expect("a fresh context is free")
}

/// Mount the volume holding `folder` if it isn't, answering prompts from the
/// saved credentials. Unknown host keys and other questions are refused. A
/// folder that doesn't exist yet is mounted through its nearest parent.
pub fn mount(nas: &Nas, folder: &gio::File, cancellable: &gio::Cancellable) -> Result<(), String> {
    let mut target = folder.clone();
    loop {
        match mount_once(nas, &target, cancellable) {
            Err((e, true)) => match target.parent() {
                Some(parent) => target = parent,
                None => return Err(e),
            },
            other => return other.map_err(|(e, _)| e),
        }
    }
}

/// Mount once; the flag says the folder itself wasn't found.
fn mount_once(nas: &Nas, folder: &gio::File, cancellable: &gio::Cancellable) -> Result<(), (String, bool)> {
    // Always ask to mount: an already mounted volume answers at once, and the
    // process's cached mount list can be stale after an unmount elsewhere.
    if folder.uri_scheme().as_deref() == Some("file") {
        return Ok(());
    }
    with_context(|ctx| {
        let op = gio::MountOperation::new();
        let password = secrets::password(&nas.id);
        let (user, domain) = (nas.username.clone(), nas.domain.clone());
        let asked = std::rc::Rc::new(std::cell::Cell::new(0));
        let tries = asked.clone();
        op.connect_ask_password(move |op, _message, _user, _domain, flags| {
            // A second ask means the saved password was refused.
            tries.set(tries.get() + 1);
            if tries.get() > 1 {
                op.reply(gio::MountOperationResult::Aborted);
                return;
            }
            if flags.contains(gio::AskPasswordFlags::NEED_USERNAME)
                && let Some(u) = &user
            {
                op.set_username(Some(u));
            }
            if flags.contains(gio::AskPasswordFlags::NEED_DOMAIN)
                && let Some(d) = &domain
            {
                op.set_domain(Some(d));
            }
            match &password {
                Some(p) if flags.contains(gio::AskPasswordFlags::NEED_PASSWORD) => op.set_password(Some(p)),
                None if flags.contains(gio::AskPasswordFlags::ANONYMOUS_SUPPORTED) && user.is_none() => op.set_anonymous(true),
                None if flags.contains(gio::AskPasswordFlags::NEED_PASSWORD) => {
                    op.reply(gio::MountOperationResult::Aborted);
                    return;
                }
                _ => {}
            }
            op.set_password_save(gio::PasswordSave::Never);
            op.reply(gio::MountOperationResult::Handled);
        });
        let questioned = std::rc::Rc::new(std::cell::Cell::new(false));
        let q = questioned.clone();
        op.connect_local("ask-question", false, move |args| {
            q.set(true);
            if let Some(op) = args.first().and_then(|v| v.get::<gio::MountOperation>().ok()) {
                op.reply(gio::MountOperationResult::Aborted);
            }
            None
        });
        let result = std::rc::Rc::new(std::cell::RefCell::new(None));
        let main = glib::MainLoop::new(Some(ctx), false);
        let (r, m) = (result.clone(), main.clone());
        // Its own cancellable, so a timeout doesn't cancel the rest of the job.
        let attempt = gio::Cancellable::new();
        let relay = attempt.clone();
        let handler = cancellable.connect_cancelled(move |_| relay.cancel());
        let timed_out = Arc::new(AtomicBool::new(false));
        let (late, give_up) = (timed_out.clone(), attempt.clone());
        let timer = glib::timeout_source_new(MOUNT_TIMEOUT, None, glib::Priority::DEFAULT, move || {
            late.store(true, Ordering::Relaxed);
            give_up.cancel();
            glib::ControlFlow::Break
        });
        timer.attach(Some(ctx));
        folder.mount_enclosing_volume(gio::MountMountFlags::NONE, Some(&op), Some(&attempt), move |res| {
            *r.borrow_mut() = Some(res);
            m.quit();
        });
        main.run();
        timer.destroy();
        if let Some(handler) = handler {
            cancellable.disconnect_cancelled(handler);
        }
        let outcome = result.borrow_mut().take();
        let plain = |m: String| (m, false);
        match outcome {
            Some(Ok(())) => Ok(()),
            Some(Err(e)) if e.matches(gio::IOErrorEnum::AlreadyMounted) => Ok(()),
            Some(Err(_)) if timed_out.load(Ordering::Relaxed) => {
                Err(plain(format!("{} didn't answer within {} seconds", nas.host(), MOUNT_TIMEOUT.as_secs())))
            }
            Some(Err(_)) if asked.get() > 1 => Err(plain(format!("{} refused the saved user name or password", nas.host()))),
            Some(Err(e)) if e.matches(gio::IOErrorEnum::FailedHandled) && secrets::password(&nas.id).is_none() => {
                Err(plain(format!("{} needs a password; add it in Share's settings", nas.host())))
            }
            Some(Err(_)) if questioned.get() => Err(plain(format!(
                "{} asked a question Share won't answer, such as whether to trust an unknown host key; connect to it once in Files",
                nas.host()
            ))),
            Some(Err(e)) if e.matches(gio::IOErrorEnum::FailedHandled) => {
                Err(plain(format!("{} asked something Share won't answer; connect to it once in Files", nas.host())))
            }
            Some(Err(e)) => {
                let missing = e.matches(gio::IOErrorEnum::NotFound) || e.message().contains("No such file or directory");
                Err((format!("Couldn't connect to {}: {}", nas.host(), e.message()), missing))
            }
            None => Err(plain("The connection was interrupted".into())),
        }
    })
}

/// Mount the NAS folder (or its Tailscale address) and make sure it exists.
fn open(nas: &Nas, sub: &[String], cancellable: &gio::Cancellable) -> Result<gio::File, String> {
    let mut last = String::new();
    for uri in std::iter::once(&nas.uri).chain(nas.tailscale_uri.iter()) {
        let base = gio::File::for_uri(uri);
        match mount(nas, &base, cancellable) {
            Ok(()) => {
                let folder = sub.iter().fold(base, |f, part| f.child(part));
                match folder.make_directory_with_parents(Some(cancellable)) {
                    Ok(()) => {}
                    Err(e) if e.matches(gio::IOErrorEnum::Exists) => {}
                    Err(e) => return Err(format!("Couldn't open the folder on {}: {}", nas.host(), e.message())),
                }
                return Ok(folder);
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Check the NAS folder can be reached and written to.
pub fn test(nas: &Nas) -> Result<String, String> {
    let cancellable = gio::Cancellable::new();
    let folder = open(nas, &[], &cancellable)?;
    let probe = folder.child(format!(".nyx-share-check-{}", uuid::Uuid::new_v4().simple()));
    let out = probe
        .create(gio::FileCreateFlags::NONE, Some(&cancellable))
        .map_err(|e| format!("The folder can't be written to: {}", e.message()))?;
    let _ = out.close(Some(&cancellable));
    let _ = probe.delete(Some(&cancellable));
    Ok(format!("{} can be reached and written to", nas.name))
}

fn copy(nas: &Nas, sub: &[String], task: &Task<'_>) -> Result<Outcome, SendError> {
    let cancellable = gio::Cancellable::new();
    let done = AtomicBool::new(false);
    std::thread::scope(|s| {
        // Mounting can take a long while, so a cancel has to reach GIO directly.
        s.spawn(|| {
            while !done.load(Ordering::Relaxed) {
                if task.cancel.load(Ordering::Relaxed) {
                    cancellable.cancel();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        });
        let result = copy_into(nas, sub, task, &cancellable);
        done.store(true, Ordering::Relaxed);
        result
    })
}

fn copy_into(nas: &Nas, sub: &[String], task: &Task<'_>, cancellable: &gio::Cancellable) -> Result<Outcome, SendError> {
    (task.step)(Some(&format!("Connecting to {}…", nas.name)));
    let opened = open(nas, sub, cancellable);
    (task.step)(None);
    let folder = match opened {
        Err(_) if task.cancelled() => return Err(SendError::Cancelled { outputs: Vec::new() }),
        other => other.map_err(SendError::failed)?,
    };
    let mut outputs = Vec::new();
    let mut skipped = 0;
    let mut tops: std::collections::HashMap<String, gio::File> = std::collections::HashMap::new();
    for item in task.items {
        if task.cancelled() {
            return Err(SendError::Cancelled { outputs });
        }
        let name = match item.info.kind {
            ContentKind::Text | ContentKind::Url => names::for_text(Some(&item.info.name)),
            _ => names::sanitize(&item.info.name),
        };
        let target = match subdir(&folder, &nyx_share_providers::folder_of(item), &mut tops, nas.conflict, cancellable) {
            Ok(dir) => dir,
            Err(e) => {
                return Err(SendError::Failed {
                    message: format!("Couldn't make a folder on the NAS: {}", e.message()),
                    outputs,
                });
            }
        };
        if nas.conflict == Conflict::Skip && target.child(&name).query_exists(Some(cancellable)) {
            skipped += 1;
            (task.progress)(item.size());
            continue;
        }
        match put(&target, &name, item, task, cancellable) {
            Ok(uri) => outputs.push(uri),
            Err(SendError::Cancelled { .. }) => return Err(SendError::Cancelled { outputs }),
            Err(SendError::Failed { message, .. }) => {
                let message = match outputs.len() {
                    0 => message,
                    n => format!("{message}. {n} of {} items were copied.", task.items.len()),
                };
                return Err(SendError::Failed { message, outputs });
            }
        }
    }
    let place = match sub {
        [] => nas.name.clone(),
        _ => format!("{} / {}", nas.name, sub.join("/")),
    };
    let only = nyx_share_providers::shared_folder(task.items).and_then(|top| tops.get(&top).and_then(|f| f.basename()));
    let message = match (outputs.len(), skipped) {
        (0, s) if s > 0 => format!("Nothing copied: {s} already in {place}"),
        (_, 0) if only.is_some() => format!("Copied {} to {place}", only.unwrap().to_string_lossy()),
        (1, 0) => format!("Copied {name} to {place}", name = item_name(&outputs[0])),
        (n, 0) => format!("Copied {n} items to {place}"),
        (n, s) => format!("Copied {n} items to {place}; {s} already there"),
    };
    Ok(Outcome { message, outputs })
}

/// The folder on the NAS for a file from a shared folder. The shared folder
/// gets a new folder ("Trip (2)" if "Trip" is taken), or with "skip files
/// already there" goes into the one of that name.
fn subdir(
    base: &gio::File,
    parts: &[String],
    tops: &mut std::collections::HashMap<String, gio::File>,
    conflict: Conflict,
    cancellable: &gio::Cancellable,
) -> Result<gio::File, glib::Error> {
    let Some((top, rest)) = parts.split_first() else { return Ok(base.clone()) };
    let exists = |e: &glib::Error| e.matches(gio::IOErrorEnum::Exists);
    let mut dir = match tops.get(top) {
        Some(made) => made.clone(),
        None => {
            let mut n = 1;
            let made = loop {
                let dir = base.child(if n == 1 { top.clone() } else { format!("{top} ({n})") });
                match dir.make_directory(Some(cancellable)) {
                    Ok(()) => break dir,
                    Err(e) if exists(&e) && conflict == Conflict::Skip => break dir,
                    Err(e) if exists(&e) && n < 10_000 => n += 1,
                    Err(e) => return Err(e),
                }
            };
            tops.insert(top.clone(), made.clone());
            made
        }
    };
    for part in rest {
        dir = dir.child(part);
        match dir.make_directory(Some(cancellable)) {
            Ok(()) => {}
            Err(e) if exists(&e) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(dir)
}

/// All of `bytes`, or the error (a short write counts as one).
fn write_all(out: &gio::FileOutputStream, bytes: &[u8], cancellable: &gio::Cancellable) -> Result<(), glib::Error> {
    match out.write_all(bytes, Some(cancellable)) {
        Ok((_, None)) => Ok(()),
        Ok((_, Some(e))) | Err(e) => Err(e),
    }
}

fn item_name(uri: &str) -> String {
    gio::File::for_uri(uri).basename().map(|b| b.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Write one item to a hidden partial file in `folder`, then rename it to the
/// first free name. The partial file is removed if anything goes wrong.
fn put(
    folder: &gio::File,
    name: &str,
    item: &nyx_share_providers::Payload,
    task: &Task<'_>,
    cancellable: &gio::Cancellable,
) -> Result<String, SendError> {
    let tag = uuid::Uuid::new_v4().simple().to_string();
    let partial = folder.child(names::partial(name, &tag[..12]));
    let fail = |e: glib::Error| SendError::failed(format!("Couldn't write to the NAS: {}", e.message()));
    let out = partial.create(gio::FileCreateFlags::NONE, Some(cancellable)).map_err(fail)?;
    let written = (|| -> Result<(), SendError> {
        match &item.data {
            Data::Text(text) => {
                write_all(&out, text.as_bytes(), cancellable).map_err(fail)?;
                (task.progress)(text.len() as u64);
            }
            Data::File(file) => {
                let mut buf = vec![0u8; 1 << 20];
                let mut offset = 0u64;
                loop {
                    if task.cancel.load(Ordering::Relaxed) {
                        cancellable.cancel();
                        return Err(SendError::Cancelled { outputs: Vec::new() });
                    }
                    let n =
                        file.read_at(&mut buf, offset).map_err(|e| SendError::failed(format!("Couldn't read the file: {e}")))?;
                    if n == 0 {
                        break;
                    }
                    write_all(&out, &buf[..n], cancellable).map_err(fail)?;
                    offset += n as u64;
                    (task.progress)(n as u64);
                }
                if offset != item.info.size {
                    return Err(SendError::failed("The file changed while it was being copied"));
                }
            }
        }
        out.close(Some(cancellable)).map_err(fail)?;
        Ok(())
    })();
    let placed = written.and_then(|()| {
        for n in 1..10_000 {
            let candidate = if n == 1 { name.to_string() } else { names::alternative(name, n) };
            let target = folder.child(&candidate);
            if target.query_exists(Some(cancellable)) {
                continue;
            }
            match partial.move_(&target, gio::FileCopyFlags::NONE, Some(cancellable), None) {
                Ok(()) => return Ok(target.uri().to_string()),
                Err(e) if e.matches(gio::IOErrorEnum::Exists) => continue,
                Err(e) => return Err(fail(e)),
            }
        }
        Err(SendError::failed(format!("Too many files are already named {name}")))
    });
    if placed.is_err() {
        // SMB won't delete a file that is still open.
        let _ = out.close(gio::Cancellable::NONE);
        let _ = partial.delete(gio::Cancellable::NONE);
    }
    placed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nas(uri: &str) -> Nas {
        Nas {
            id: "x".into(),
            name: "NAS Documents".into(),
            uri: uri.into(),
            tailscale_uri: None,
            username: None,
            domain: None,
            conflict: Conflict::KeepBoth,
        }
    }

    #[test]
    fn reads_protocol_and_host() {
        assert_eq!(nas("smb://nas.local/Documents/Inbox").protocol(), "SMB");
        assert_eq!(nas("smb://nas.local/Documents/Inbox").host(), "nas.local");
        assert_eq!(nas("sftp://ken@homelab:2222/srv").host(), "homelab:2222");
        assert_eq!(nas("nfs://nas/export").protocol(), "NFS");
        assert!(nas("http://x/y").validate().is_err());
        assert!(nas("smb://x/a/../b").validate().is_err());
        assert!(nas("smb://x/a").validate().is_ok());
    }

    #[test]
    fn cleans_subfolders() {
        assert_eq!(clean_subfolder("2026/ Trips /").unwrap(), ["2026", "Trips"]);
        assert!(clean_subfolder("a/../b").is_err());
        assert_eq!(clean_subfolder("").unwrap(), Vec::<String>::new());
    }
}
