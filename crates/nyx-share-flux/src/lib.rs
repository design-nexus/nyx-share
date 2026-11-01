//! Phones and tablets paired in Flux, as Share destinations. Flux's `fluxd`
//! owns pairing and the connection; Share only asks it to send, over its
//! socket of JSON lines (`{"id":1,"method":"share.url","params":{…}}`).

use nyx_share_core::{ContentKind, Destination, Section};
use nyx_share_providers::stage::Stage;
use nyx_share_providers::{Data, Outcome, Payload, Provider, SendError, Task};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const PREFIX: &str = "flux:";
/// Flux's own limits: files waiting in its outbox, and text sent to a device.
const MAX_FILES: u32 = 100;
const MAX_TEXT: usize = 256 << 10;
/// How long the device list is reused before fluxd is asked again.
const FRESH: Duration = Duration::from_secs(3);
/// Flux copies each file into its outbox before it answers `share.files`.
const HAND_OVER: Duration = Duration::from_secs(15 * 60);
/// A transfer Flux never lists is given up after this long.
const LOST: Duration = Duration::from_secs(30);

/// Where fluxd listens: `$FLUX_SOCKET`, else `$XDG_RUNTIME_DIR/flux/fluxd.sock`.
pub fn socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("FLUX_SOCKET").filter(|p| !p.is_empty()) {
        return path.into();
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        // SAFETY: getuid has no preconditions.
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })));
    runtime.join("flux/fluxd.sock")
}

#[derive(Debug, Clone, PartialEq)]
pub struct FluxError {
    pub code: String,
    pub message: String,
}

impl FluxError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into() }
    }
}

/// One connection to fluxd. Replies are matched by id; events are skipped.
pub struct Conn {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next: i64,
}

impl Conn {
    pub fn open(path: &Path) -> Result<Self, FluxError> {
        let stream = UnixStream::connect(path).map_err(|_| FluxError::new("no_flux", "Flux isn't running"))?;
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
        let reader = stream.try_clone().map_err(|e| FluxError::new("io", e.to_string()))?;
        let conn = Self { reader: BufReader::new(reader), writer: stream, next: 1 };
        conn.wait_at_most(Duration::from_secs(10));
        Ok(conn)
    }

    pub fn wait_at_most(&self, timeout: Duration) {
        let _ = self.writer.set_read_timeout(Some(timeout));
    }

    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, FluxError> {
        let id = self.next;
        self.next += 1;
        let line = format!("{}\n", json!({ "id": id, "method": method, "params": params }));
        let lost = |e: std::io::Error| FluxError::new("io", format!("Lost the connection to Flux: {e}"));
        self.writer.write_all(line.as_bytes()).map_err(lost)?;
        loop {
            let mut reply = String::new();
            if self.reader.read_line(&mut reply).map_err(lost)? == 0 {
                return Err(FluxError::new("io", "Flux closed the connection"));
            }
            let Ok(msg) = serde_json::from_str::<Value>(&reply) else { continue };
            if msg.get("id").and_then(Value::as_i64) != Some(id) {
                continue;
            }
            if let Some(e) = msg.get("error") {
                return Err(FluxError::new(
                    e.get("code").and_then(Value::as_str).unwrap_or("error"),
                    e.get("message").and_then(Value::as_str).unwrap_or("Flux refused"),
                ));
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub paired: bool,
    #[serde(default)]
    pub online: bool,
    #[serde(default)]
    pub plugins: Vec<String>,
}

impl Device {
    /// Paired, and the device takes shares.
    fn usable(&self) -> bool {
        self.paired && self.plugins.iter().any(|p| p == "share")
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
struct Transfer {
    id: String,
    #[serde(default)]
    done: i64,
    #[serde(default)]
    state: String,
    #[serde(default)]
    error: Option<String>,
}

impl Transfer {
    fn finished(&self) -> bool {
        matches!(self.state.as_str(), "done" | "failed" | "canceled")
    }
}

#[derive(Default, Deserialize)]
struct Snapshot {
    #[serde(default)]
    devices: Vec<Device>,
    #[serde(default)]
    transfers: Vec<Transfer>,
}

fn snapshot(conn: &mut Conn) -> Result<Snapshot, FluxError> {
    let value = conn.call("state", json!({}))?;
    serde_json::from_value(value).map_err(|e| FluxError::new("bad_reply", format!("Flux answered oddly: {e}")))
}

pub struct FluxProvider {
    socket: PathBuf,
    /// Where files are staged for Flux: `$XDG_CACHE_HOME/nyx-share/flux`.
    stage: PathBuf,
    cache: Mutex<Option<(Instant, Vec<Device>)>>,
}

impl FluxProvider {
    pub fn new(socket: PathBuf) -> Self {
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache"));
        Self::with_stage(socket, cache.join("nyx-share/flux"))
    }

    pub fn with_stage(socket: PathBuf, stage: PathBuf) -> Self {
        Self { socket, stage, cache: Mutex::new(None) }
    }

    fn devices(&self) -> Vec<Device> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, list)) = cache.as_ref()
            && at.elapsed() < FRESH
        {
            return list.clone();
        }
        // Without Flux running there is nothing to list; ask again later.
        let list = Conn::open(&self.socket).and_then(|mut c| snapshot(&mut c)).map(|s| s.devices).unwrap_or_default();
        *cache = Some((Instant::now(), list.clone()));
        list
    }
}

fn icon(kind: &str) -> &'static str {
    match kind {
        "phone" | "tablet" => "phone-symbolic",
        "tv" => "video-display-symbolic",
        _ => "computer-symbolic",
    }
}

impl Provider for FluxProvider {
    fn id(&self) -> &'static str {
        "flux"
    }

    fn name(&self) -> &'static str {
        "Flux"
    }

    fn kinds(&self) -> Vec<ContentKind> {
        vec![ContentKind::Text, ContentKind::Url, ContentKind::Image, ContentKind::File]
    }

    fn destinations(&self) -> Vec<Destination> {
        self.devices()
            .into_iter()
            .filter(Device::usable)
            .map(|d| Destination {
                id: format!("{PREFIX}{}", d.id),
                provider: self.id().into(),
                name: d.name.clone(),
                icon: icon(&d.kind).into(),
                tint: "green".into(),
                section: Section::Devices,
                kinds: self.kinds(),
                max_items: Some(MAX_FILES),
                needs_folder: false,
                // Files wait in Flux's outbox for an offline device.
                available: true,
                status: Some(if d.online { "Flux".into() } else { "Offline · Flux sends files when it's back".into() }),
                pinned: false,
                hidden: false,
                default: false,
            })
            .collect()
    }

    fn send(&self, destination: &str, task: &Task<'_>) -> Result<Outcome, SendError> {
        let id = destination.strip_prefix(PREFIX).unwrap_or(destination);
        let failed = |e: FluxError| SendError::failed(e.message);
        let mut conn = Conn::open(&self.socket).map_err(failed)?;
        let state = snapshot(&mut conn).map_err(failed)?;
        let device = state
            .devices
            .into_iter()
            .find(|d| d.id == id && d.paired)
            .ok_or_else(|| SendError::failed("That device isn't paired in Flux any more"))?;
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) = None;

        let (texts, files): (Vec<&Payload>, Vec<&Payload>) = task.items.iter().partition(|p| matches!(p.data, Data::Text(_)));
        let mut sent = 0;
        for item in &texts {
            if task.cancelled() {
                return Err(SendError::Cancelled { outputs: Vec::new() });
            }
            let Data::Text(text) = &item.data else { continue };
            if text.len() > MAX_TEXT {
                return Err(SendError::failed(format!("Flux sends at most {} KiB of text at a time", MAX_TEXT >> 10)));
            }
            let web = item.info.kind == ContentKind::Url && (text.starts_with("https://") || text.starts_with("http://"));
            let result = match web {
                true => conn.call("share.url", json!({ "device": device.id, "url": text })),
                false => conn.call("share.text", json!({ "device": device.id, "text": text })),
            };
            result.map_err(|e| partial(e, sent, task.items.len()))?;
            sent += 1;
            (task.progress)(text.len() as u64);
        }
        if files.is_empty() {
            return Ok(Outcome { message: sent_message(task.items, &device.name), outputs: Vec::new() });
        }

        // Flux takes paths, and the phone sees each file's name: stage each item
        // under its own name, then let Flux copy it into its outbox.
        (task.step)(Some(&format!("Handing over to Flux for {}…", device.name)));
        let mut stage = Stage::new(&self.stage).map_err(|e| SendError::failed(format!("Couldn't prepare the files: {e}")))?;
        let mut paths = Vec::new();
        for item in &files {
            if task.cancelled() {
                return Err(SendError::Cancelled { outputs: Vec::new() });
            }
            let Data::File(file) = &item.data else { continue };
            let path = stage.place(file, &item.info.name, item.info.size, task)?;
            paths.push(path);
        }
        conn.wait_at_most(HAND_OVER);
        let reply = conn
            .call("share.files", json!({ "device": device.id, "paths": paths }))
            .map_err(|e| partial(e, sent, task.items.len()))?;
        conn.wait_at_most(Duration::from_secs(10));
        (task.step)(None);
        let ids: Vec<String> = reply
            .get("transfers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        let result = follow(&mut conn, &device, &ids, task);
        drop(stage);
        result.map(|waiting| Outcome {
            message: match waiting {
                0 => sent_message(task.items, &device.name),
                n => format!(
                    "Waiting in Flux: {} {} when {} is back",
                    n,
                    if n == 1 { "file goes" } else { "files go" },
                    device.name
                ),
            },
            outputs: Vec::new(),
        })
    }
}

/// A failure after some items already went.
fn partial(e: FluxError, sent: usize, total: usize) -> SendError {
    match sent {
        0 => SendError::failed(e.message),
        n => SendError::failed(format!("{}. {n} of {total} items were sent.", e.message)),
    }
}

fn sent_message(items: &[Payload], device: &str) -> String {
    match items {
        [one] if one.info.kind == ContentKind::Url => format!("Sent the link to {device}"),
        [one] if one.info.kind == ContentKind::Text => format!("Sent the text to {device}"),
        [one] => format!("Sent {} to {device}", one.info.name),
        many => format!("Sent {} items to {device}", many.len()),
    }
}

/// Follow Flux's transfers until they end, passing on progress. Returns how
/// many still wait in the outbox for an offline device.
fn follow(conn: &mut Conn, device: &Device, ids: &[String], task: &Task<'_>) -> Result<usize, SendError> {
    let mut reported: HashMap<String, i64> = HashMap::new();
    let started = Instant::now();
    loop {
        let state = snapshot(conn).map_err(|e| SendError::failed(e.message))?;
        if !ids.iter().any(|id| state.transfers.iter().any(|t| &t.id == id)) && started.elapsed() > LOST {
            return Err(SendError::failed("Flux lost track of the transfer"));
        }
        let mine: Vec<Transfer> = ids
            .iter()
            .map(|id| {
                state
                    .transfers
                    .iter()
                    .find(|t| &t.id == id)
                    .cloned()
                    .unwrap_or_else(|| Transfer { id: id.clone(), ..Default::default() })
            })
            .collect();
        if task.cancelled() {
            for t in mine.iter().filter(|t| !t.finished()) {
                let _ = conn.call("transfer.cancel", json!({ "id": t.id }));
            }
            return Err(SendError::Cancelled { outputs: Vec::new() });
        }
        for t in &mine {
            let before = reported.insert(t.id.clone(), t.done).unwrap_or(0);
            if t.done > before {
                (task.progress)((t.done - before) as u64);
            }
        }
        if let Some(t) = mine.iter().find(|t| t.state == "failed") {
            let done = mine.iter().filter(|t| t.state == "done").count();
            let reason = t.error.clone().unwrap_or_else(|| "the transfer stopped".into());
            let message = match done {
                0 => format!("Flux couldn't send to {}: {reason}", device.name),
                n => format!("Flux couldn't send everything to {}: {reason}. {n} of {} files arrived.", device.name, mine.len()),
            };
            return Err(SendError::failed(message));
        }
        if mine.iter().any(|t| t.state == "canceled") && mine.iter().all(Transfer::finished) {
            return Err(SendError::Cancelled { outputs: Vec::new() });
        }
        if mine.iter().all(Transfer::finished) {
            return Ok(0);
        }
        let online = state.devices.iter().any(|d| d.id == device.id && d.online);
        let pending = mine.iter().filter(|t| matches!(t.state.as_str(), "waiting" | "queued" | "")).count();
        if !online && pending == mine.iter().filter(|t| !t.finished()).count() {
            return Ok(pending);
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

#[cfg(test)]
mod tests;
