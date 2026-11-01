//! Third-party providers. Each lives in its own folder,
//! `$XDG_DATA_HOME/nyx-share/providers/<id>/provider.toml`, and never loads into
//! the service: every send runs its program in a bubblewrap sandbox with no
//! access to the user's files, the session bus or the display, and no network
//! unless its manifest asks for it. The items of that one send reach it as
//! open file descriptors; it answers in JSON lines.
//!
//! ```toml
//! id = "word-count"            # the folder's name
//! name = "Word count"
//! exec = "word-count"          # a program inside the folder
//! kinds = ["text", "file"]     # text, url, image, file
//! max_items = 10               # optional
//! icon = "accessories-text-editor-symbolic"   # optional
//! [permissions]
//! network = false              # the default
//! ```

use nyx_share_core::{ContentKind, Destination, Section};
use nyx_share_providers::{Data, Outcome, Provider, SendError, Task};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const PREFIX: &str = "ext:";
/// The first descriptor number an item gets inside the sandbox.
const FIRST_FD: i32 = 3;
const FRESH: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    id: String,
    name: String,
    exec: String,
    kinds: Vec<String>,
    #[serde(default)]
    max_items: Option<u32>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    permissions: Permissions,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Permissions {
    #[serde(default)]
    network: bool,
}

/// A provider whose manifest checked out.
#[derive(Clone, Debug)]
pub struct Installed {
    pub id: String,
    pub name: String,
    pub dir: PathBuf,
    /// The program, relative to `dir`.
    pub exec: PathBuf,
    pub kinds: Vec<ContentKind>,
    pub max_items: Option<u32>,
    pub icon: String,
    pub network: bool,
}

fn valid_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= 63
        && b[0].is_ascii_alphanumeric()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

fn kind(name: &str) -> Option<ContentKind> {
    Some(match name {
        "text" => ContentKind::Text,
        "url" => ContentKind::Url,
        "image" => ContentKind::Image,
        "file" => ContentKind::File,
        _ => return None,
    })
}

/// Read one provider folder; the error says what is wrong with it.
pub fn read(dir: &Path) -> Result<Installed, String> {
    let folder = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
    let text = std::fs::read_to_string(dir.join("provider.toml")).map_err(|e| format!("{folder}: no provider.toml ({e})"))?;
    let m: Manifest = toml::from_str(&text).map_err(|e| format!("{folder}: {e}"))?;
    if !valid_id(&m.id) || m.id != folder {
        return Err(format!("{folder}: the id must be the folder's name, in lowercase letters, digits and dashes"));
    }
    let name = m.name.trim();
    if name.is_empty() || name.chars().count() > 60 || name.chars().any(char::is_control) {
        return Err(format!("{folder}: the name must be 1 to 60 characters"));
    }
    let exec = PathBuf::from(&m.exec);
    if exec.is_absolute() || exec.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        return Err(format!("{folder}: exec must be a program inside the folder"));
    }
    let program = dir.join(&exec);
    let real = program.canonicalize().map_err(|_| format!("{folder}: {} isn't there", m.exec))?;
    let base = dir.canonicalize().map_err(|e| format!("{folder}: {e}"))?;
    let meta = std::fs::metadata(&real).map_err(|e| format!("{folder}: {e}"))?;
    if !real.starts_with(&base) || !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
        return Err(format!("{folder}: {} must be an executable file inside the folder", m.exec));
    }
    let kinds: Vec<ContentKind> =
        m.kinds.iter().map(|k| kind(k).ok_or_else(|| format!("{folder}: unknown kind {k}"))).collect::<Result<_, _>>()?;
    if kinds.is_empty() {
        return Err(format!("{folder}: kinds can't be empty"));
    }
    Ok(Installed {
        id: m.id,
        name: name.to_string(),
        dir: base,
        exec,
        kinds,
        max_items: m.max_items.filter(|n| *n > 0),
        icon: m
            .icon
            .filter(|i| i.ends_with("-symbolic") && i.len() < 100)
            .unwrap_or_else(|| "application-x-addon-symbolic".into()),
        network: m.permissions.network,
    })
}

/// Every provider under `root`, and what is wrong with the ones that don't load.
pub fn scan(root: &Path) -> (Vec<Installed>, Vec<String>) {
    let (mut ok, mut problems) = (Vec::new(), Vec::new());
    let Ok(entries) = std::fs::read_dir(root) else { return (ok, problems) };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for dir in dirs {
        match read(&dir) {
            Ok(p) => ok.push(p),
            Err(e) => problems.push(e),
        }
    }
    (ok, problems)
}

pub struct External {
    root: PathBuf,
    data: PathBuf,
    cache: Mutex<Option<(Instant, Vec<Installed>)>>,
}

impl Default for External {
    fn default() -> Self {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"));
        Self::at(data.join("nyx-share/providers"), data.join("nyx-share/provider-data"))
    }
}

impl External {
    /// Providers in `root`; each keeps its own files in `data/<id>`.
    pub fn at(root: PathBuf, data: PathBuf) -> Self {
        Self { root, data, cache: Mutex::new(None) }
    }

    fn installed(&self) -> Vec<Installed> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, list)) = cache.as_ref()
            && at.elapsed() < FRESH
        {
            return list.clone();
        }
        let (list, problems) = scan(&self.root);
        for p in problems {
            eprintln!("nyx-share: provider skipped: {p}");
        }
        *cache = Some((Instant::now(), list.clone()));
        list
    }
}

/// The sandbox: read-only system, the provider's folder at /provider, its own
/// data at /data, nothing of the user's, a private network unless allowed.
fn sandbox(bwrap: &Path, p: &Installed, data: &Path) -> Command {
    let mut c = Command::new(bwrap);
    c.args(["--unshare-all", "--die-with-parent", "--new-session", "--clearenv"]);
    if p.network {
        c.arg("--share-net");
    }
    c.args(["--setenv", "PATH", "/usr/bin", "--setenv", "HOME", "/data", "--setenv", "LANG", "C.UTF-8"]);
    c.args(["--ro-bind", "/usr", "/usr", "--ro-bind", "/etc", "/etc"]);
    c.args([
        "--symlink",
        "usr/bin",
        "/bin",
        "--symlink",
        "usr/bin",
        "/sbin",
        "--symlink",
        "usr/lib",
        "/lib",
        "--symlink",
        "usr/lib",
        "/lib64",
    ]);
    c.args(["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp", "--tmpfs", "/run"]);
    c.arg("--bind").arg(data).arg("/data");
    c.arg("--ro-bind").arg(&p.dir).arg("/provider");
    c.args(["--chdir", "/data", "--"]);
    c.arg(Path::new("/provider").join(&p.exec));
    c
}

/// A line from the provider, cut to something fit to show.
fn sentence(v: &Value) -> Option<String> {
    let s: String = v.as_str()?.chars().filter(|c| !c.is_control()).take(300).collect();
    Some(s).filter(|s| !s.trim().is_empty())
}

impl Provider for External {
    fn id(&self) -> &'static str {
        "external"
    }

    fn name(&self) -> &'static str {
        "Extensions"
    }

    fn kinds(&self) -> Vec<ContentKind> {
        vec![ContentKind::Text, ContentKind::Url, ContentKind::Image, ContentKind::File]
    }

    fn destinations(&self) -> Vec<Destination> {
        let sandboxed = nyx_share_providers::process::find("bwrap").is_some();
        self.installed()
            .into_iter()
            .map(|p| Destination {
                id: format!("{PREFIX}{}", p.id),
                provider: self.id().into(),
                name: p.name.clone(),
                icon: p.icon.clone(),
                tint: "graphite".into(),
                section: Section::Apps,
                kinds: p.kinds.clone(),
                max_items: p.max_items,
                needs_folder: false,
                needs_recipient: false,
                available: sandboxed,
                status: Some(match (sandboxed, p.network) {
                    (false, _) => "Needs bubblewrap (bwrap)".into(),
                    (true, true) => "Extension · uses the network".into(),
                    (true, false) => "Extension".into(),
                }),
                pinned: false,
                hidden: false,
                default: false,
            })
            .collect()
    }

    fn send(&self, destination: &str, task: &Task<'_>) -> Result<Outcome, SendError> {
        let failed = SendError::failed;
        let id = destination.strip_prefix(PREFIX).unwrap_or(destination);
        let p = self
            .installed()
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| failed("That extension isn't installed any more".into()))?;
        let bwrap =
            nyx_share_providers::process::find("bwrap").ok_or_else(|| failed("Extensions need bubblewrap (bwrap)".into()))?;
        let data = self.data.join(&p.id);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&data)
            .map_err(|e| failed(format!("Couldn't make its folder: {e}")))?;

        // Only this send's items, as descriptors 3, 4, … inside the sandbox.
        let mut sources = Vec::new();
        let items: Vec<Value> = task
            .items
            .iter()
            .map(|item| {
                let kind = serde_json::to_value(item.info.kind).unwrap_or(Value::Null);
                match &item.data {
                    Data::Text(text) => json!({ "kind": kind, "name": item.info.name, "text": text }),
                    Data::File(file) => {
                        let fd = FIRST_FD + sources.len() as i32;
                        sources.push(file.as_raw_fd());
                        json!({ "kind": kind, "name": item.info.name, "mime": item.info.mime, "size": item.info.size, "fd": fd })
                    }
                }
            })
            .collect();
        let request = json!({ "version": 1, "method": "send", "items": items });

        let mut command = sandbox(&bwrap, &p, &data);
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        // SAFETY: only async-signal-safe calls (fcntl, dup2) between fork and exec.
        // The descriptors are first moved out of the way, so placing them at
        // 3, 4, … can't overwrite one not yet placed.
        unsafe {
            command.pre_exec(move || {
                let mut high = Vec::with_capacity(sources.len());
                for fd in &sources {
                    let h = libc::fcntl(*fd, libc::F_DUPFD, 1000);
                    if h < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    high.push(h);
                }
                for (i, h) in high.iter().enumerate() {
                    if libc::dup2(*h, FIRST_FD + i as i32) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    libc::close(*h);
                }
                Ok(())
            });
        }
        let mut child = command.spawn().map_err(|e| failed(format!("Couldn't start {}: {e}", p.name)))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = writeln!(stdin, "{request}");
        }
        let stdout = child.stdout.take().expect("piped");
        let mut stderr = child.stderr.take().expect("piped");
        let pid = child.id() as i32;
        let finished = AtomicBool::new(false);
        let total: u64 = task.items.iter().map(|i| i.size()).sum();

        let answer = std::thread::scope(|s| {
            // Cancelling ends the sandbox (and with it the provider).
            s.spawn(|| {
                while !finished.load(Ordering::Relaxed) {
                    if task.cancelled() {
                        // SAFETY: signalling our own child process.
                        unsafe { libc::kill(pid, libc::SIGKILL) };
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            });
            let err_tail = s.spawn(move || {
                let mut buf = Vec::new();
                let _ = stderr.read_to_end(&mut buf);
                String::from_utf8_lossy(&buf[buf.len().saturating_sub(400)..]).trim().to_string()
            });
            let mut answer: Option<Result<String, String>> = None;
            let mut sent = 0u64;
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                if let Some(n) = v.get("progress").and_then(Value::as_u64) {
                    let n = n.min(total.saturating_sub(sent));
                    sent += n;
                    (task.progress)(n);
                } else if let Some(step) = v.get("step") {
                    (task.step)(sentence(step).as_deref());
                } else if let Some(e) = v.get("error") {
                    answer = Some(Err(sentence(e).unwrap_or_else(|| "it failed".into())));
                } else if let Some(done) = v.get("done") {
                    answer = Some(Ok(sentence(&done["message"]).unwrap_or_else(|| format!("Sent to {}", p.name))));
                }
            }
            let _ = child.wait();
            finished.store(true, Ordering::Relaxed);
            (answer, err_tail.join().unwrap_or_default())
        });
        if task.cancelled() {
            return Err(SendError::Cancelled { outputs: Vec::new() });
        }
        match answer {
            (Some(Ok(message)), _) => Ok(Outcome { message, outputs: Vec::new() }),
            (Some(Err(e)), _) => Err(failed(format!("{}: {e}", p.name))),
            (None, tail) if tail.is_empty() => Err(failed(format!("{} stopped without an answer", p.name))),
            (None, tail) => Err(failed(format!("{} stopped without an answer: {tail}", p.name))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifests_are_checked() {
        let root = std::env::temp_dir().join(format!("nyx-share-ext-{}", std::process::id()));
        let make = |id: &str, manifest: &str, exec: Option<&str>| {
            let dir = root.join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("provider.toml"), manifest).unwrap();
            if let Some(e) = exec {
                std::fs::write(dir.join(e), "#!/bin/sh\n").unwrap();
                std::fs::set_permissions(dir.join(e), std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            dir
        };
        assert!(read(&make("good", "id = \"good\"\nname = \"Good\"\nexec = \"run\"\nkinds = [\"text\"]\n", Some("run"))).is_ok());
        assert!(
            read(&make("other", "id = \"good\"\nname = \"Good\"\nexec = \"run\"\nkinds = [\"text\"]\n", Some("run"))).is_err()
        );
        assert!(
            read(&make("escape", "id = \"escape\"\nname = \"E\"\nexec = \"../good/run\"\nkinds = [\"text\"]\n", None)).is_err()
        );
        assert!(
            read(&make("kinds", "id = \"kinds\"\nname = \"K\"\nexec = \"run\"\nkinds = [\"everything\"]\n", Some("run")))
                .is_err()
        );
        assert!(
            read(&make(
                "extra",
                "id = \"extra\"\nname = \"X\"\nexec = \"run\"\nkinds = [\"text\"]\n[permissions]\nfiles = true\n",
                Some("run")
            ))
            .is_err()
        );
        let (ok, problems) = scan(&root);
        assert_eq!(ok.len(), 1);
        assert_eq!(problems.len(), 4);
        std::fs::remove_dir_all(root).unwrap();
    }
}
