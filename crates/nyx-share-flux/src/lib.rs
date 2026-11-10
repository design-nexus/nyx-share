//! Phones and tablets paired in Flux, as Share destinations. Flux's `fluxd`
//! owns pairing and the connection; Share only asks it to send, through
//! Nyx Connect (`nyx-connect`), and follows what it receives ([`incoming`]).

use nyx_connect::{Conn, Device, Error as FluxError, Event, Feature, Snapshot, Transfer, Watch};
use nyx_share_core::{ContentKind, Destination, Recipient, Section};
use nyx_share_providers::stage::Stage;
use nyx_share_providers::{Data, Outcome, Payload, Provider, SendError, Task};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub mod incoming;

pub use nyx_connect::socket_path;

pub const PREFIX: &str = "flux:";
/// A text message through a phone paired in Flux.
pub const SMS_PREFIX: &str = "flux-sms:";
/// The longest text message Share sends; the phone splits it into parts.
const MAX_SMS: usize = 2000;
/// Flux's own limits: files waiting in its outbox, and text sent to a device.
const MAX_FILES: u32 = 100;
const MAX_TEXT: usize = 256 << 10;
/// How long the device list is reused before fluxd is asked again.
const FRESH: Duration = Duration::from_secs(3);
/// Flux copies each file into its outbox before it answers `share.files`.
const HAND_OVER: Duration = Duration::from_secs(15 * 60);
/// A transfer Flux never lists is given up after this long.
const LOST: Duration = Duration::from_secs(30);

/// Paired, and the device takes shares.
fn usable(d: &Device) -> bool {
    d.paired && d.has(Feature::Share)
}

/// Paired, and the phone sends text messages for this computer.
fn texts(d: &Device) -> bool {
    d.paired && d.has(Feature::Sms)
}

fn snapshot(conn: &mut Conn) -> Result<Snapshot, FluxError> {
    conn.state()
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
        let devices = self.devices();
        let texts = devices.iter().filter(|d| texts(d)).map(|d| Destination {
            id: format!("{SMS_PREFIX}{}", d.id),
            provider: self.id().into(),
            name: "Text message".into(),
            icon: "chat-message-new-symbolic".into(),
            tint: "green".into(),
            section: Section::Apps,
            kinds: vec![ContentKind::Text, ContentKind::Url],
            max_items: Some(5),
            needs_folder: false,
            needs_recipient: true,
            // Flux sends a text only while the phone is connected.
            available: d.online,
            status: Some(if d.online { format!("From {}", d.name) } else { format!("{} is offline", d.name) }),
            pinned: false,
            hidden: false,
            default: false,
        });
        let texts: Vec<Destination> = texts.collect();
        devices
            .into_iter()
            .filter(usable)
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
                needs_recipient: false,
                // Files wait in Flux's outbox for an offline device.
                available: true,
                status: Some(if d.online { "Flux".into() } else { "Offline · Flux sends files when it's back".into() }),
                pinned: false,
                hidden: false,
                default: false,
            })
            .chain(texts)
            .collect()
    }

    fn recipients(&self, destination: &str) -> Vec<Recipient> {
        let Some(id) = destination.strip_prefix(SMS_PREFIX) else { return Vec::new() };
        let Some(device) = self.devices().into_iter().find(|d| d.id == id) else { return Vec::new() };
        let mut out: Vec<Recipient> = Vec::new();
        // One-to-one conversations only; Flux for Android texts one address.
        for c in device.conversations.iter().filter(|c| c.addresses.len() <= 1 && !c.address.is_empty()) {
            if !out.iter().any(|r| r.address == c.address) {
                let name = if c.name.trim().is_empty() { c.address.clone() } else { c.name.clone() };
                out.push(Recipient { name, address: c.address.clone() });
            }
        }
        out.truncate(20);
        out
    }

    fn send(&self, destination: &str, task: &Task<'_>) -> Result<Outcome, SendError> {
        if let Some(id) = destination.strip_prefix(SMS_PREFIX) {
            return self.text(id, task);
        }
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
        let result = follow(&mut conn, &self.socket, &device, &ids, task);
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

impl FluxProvider {
    /// Send the items' text as one text message through the phone.
    fn text(&self, id: &str, task: &Task<'_>) -> Result<Outcome, SendError> {
        let to = task.options.recipient.as_deref().ok_or_else(|| SendError::failed("Choose who to text"))?;
        let body: Vec<&str> =
            task.items.iter().filter_map(|p| if let Data::Text(t) = &p.data { Some(t.trim()) } else { None }).collect();
        if body.len() != task.items.len() {
            return Err(SendError::failed("A text message takes only text and links"));
        }
        let body = body.join("\n");
        if body.is_empty() {
            return Err(SendError::failed("There's nothing to text"));
        }
        if body.chars().count() > MAX_SMS {
            return Err(SendError::failed(format!("A text message is at most {MAX_SMS} characters")));
        }
        let failed = |e: FluxError| SendError::failed(e.message);
        let mut conn = Conn::open(&self.socket).map_err(failed)?;
        let device = snapshot(&mut conn)
            .map_err(failed)?
            .devices
            .into_iter()
            .find(|d| d.id == id && texts(d))
            .ok_or_else(|| SendError::failed("That phone doesn't send text messages through Flux any more"))?;
        if !device.online {
            return Err(SendError::failed(format!(
                "{} is offline. Flux sends a text only while the phone is connected.",
                device.name
            )));
        }
        if task.cancelled() {
            return Err(SendError::Cancelled { outputs: Vec::new() });
        }
        let who = device
            .conversations
            .iter()
            .find(|c| c.address == to && !c.name.trim().is_empty())
            .map_or_else(|| to.to_string(), |c| c.name.clone());
        conn.call("sms.send", json!({ "device": device.id, "addresses": [to], "body": body })).map_err(|e| match e.kind {
            nyx_connect::ErrorKind::Unsupported => SendError::failed(format!("{} can't send that text", device.name)),
            _ => failed(e),
        })?;
        (task.progress)(body.len() as u64);
        Ok(Outcome { message: format!("{} is texting {who}", device.name), outputs: Vec::new() })
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
fn follow(
    conn: &mut Conn,
    socket: &std::path::Path,
    device: &Device,
    ids: &[String],
    task: &Task<'_>,
) -> Result<usize, SendError> {
    let (tx, rx) = mpsc::channel();
    let _watch = Watch::at(socket.to_path_buf(), move |e| {
        let _ = tx.send(e);
    });
    let mut reported: HashMap<String, i64> = HashMap::new();
    let mut mine: Vec<Transfer> = Vec::new();
    let started = Instant::now();
    loop {
        if task.cancelled() {
            for t in mine.iter().filter(|t| !t.finished()) {
                let _ = conn.call("transfer.cancel", json!({ "id": t.id }));
            }
            if mine.is_empty() {
                for id in ids {
                    let _ = conn.call("transfer.cancel", json!({ "id": id }));
                }
            }
            return Err(SendError::Cancelled { outputs: Vec::new() });
        }
        let state = match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(Event::State(state)) => state,
            Ok(Event::Unavailable(e)) => {
                return Err(SendError::failed(format!("{e}. The files wait in Flux's outbox until it's back.")));
            }
            Ok(Event::Toast(_)) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if mine.is_empty() && started.elapsed() > LOST {
                    return Err(SendError::failed("Flux lost track of the transfer"));
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(SendError::failed("Lost the connection to Flux")),
        };
        if !ids.iter().any(|id| state.transfers.iter().any(|t| &t.id == id)) {
            if started.elapsed() > LOST {
                return Err(SendError::failed("Flux lost track of the transfer"));
            }
            continue;
        }
        mine = ids
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
        for t in &mine {
            let before = reported.insert(t.id.clone(), t.done).unwrap_or(0);
            if t.done > before {
                (task.progress)((t.done - before) as u64);
            }
        }
        if let Some(t) = mine.iter().find(|t| t.state == "failed") {
            let done = mine.iter().filter(|t| t.state == "done").count();
            let reason = if t.error.is_empty() { "the transfer stopped".to_string() } else { t.error.clone() };
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
    }
}

#[cfg(test)]
mod tests;
