//! `nyx-share files-provider`: the Nyx Files provider (protocol 1, see
//! nyx-files/docs/file-providers.md). Adds Share… to the item menu and the
//! service's ready-to-send destinations to Send to… → Share.

use nyx_share_client::{Client, Content, ContentKind, Error, JobState, ShareOptions};
use serde_json::{Value, json};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::{Duration, Instant};

const SHEET: &str = "share";
/// Files gives a request eight seconds; a quick destination is reported as done
/// if it finishes within this, otherwise as a job still running.
const QUICK: Duration = Duration::from_secs(4);

pub fn run() -> std::process::ExitCode {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    let mut client: Option<Client> = None;
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(request) = serde_json::from_str::<Value>(&line) else { continue };
        let reply = handle(&request, &mut client);
        if writeln!(stdout, "{reply}").and_then(|()| stdout.flush()).is_err() {
            break;
        }
    }
    std::process::ExitCode::SUCCESS
}

fn handle(request: &Value, client: &mut Option<Client>) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let paths: Vec<String> = request
        .get("paths")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| p.as_str().map(String::from))
        .collect();
    let background = request.get("background").and_then(Value::as_bool).unwrap_or(false);
    match request.get("method").and_then(Value::as_str) {
        Some("query") => json!({ "id": id, "decorations": [] }),
        Some("menu") => json!({ "id": id, "actions": menu(&paths, background, client) }),
        Some("activate") => {
            let action = request.get("action").and_then(Value::as_str).unwrap_or_default();
            let context = request.get("context").and_then(Value::as_str).unwrap_or_default();
            let mut reply = activate(&paths, action, context, client);
            reply["id"] = id;
            reply
        }
        _ => json!({ "id": id, "error": "unsupported-method" }),
    }
}

fn connected(client: &mut Option<Client>) -> Result<&Client, Error> {
    if client.is_none() {
        *client = Some(Client::connect()?);
    }
    Ok(client.as_ref().expect("just connected"))
}

/// The selection's kinds, or None if anything in it can't be shared (folders,
/// special files, unreadable paths).
fn kinds(paths: &[String]) -> Option<Vec<ContentKind>> {
    paths
        .iter()
        .map(|p| {
            std::fs::metadata(p).ok().filter(|m| m.is_file())?;
            let image = nyx_share_core::mime::guess(p).is_some_and(|m| m.starts_with("image/"));
            Some(if image { ContentKind::Image } else { ContentKind::File })
        })
        .collect()
}

/// Ties an action to the files it was offered for: their device, inode and
/// modification time, in selection order.
fn identity(paths: &[String]) -> Option<String> {
    let mut h = DefaultHasher::new();
    for p in paths {
        let m = std::fs::metadata(p).ok()?;
        (p, m.dev(), m.ino(), m.mtime(), m.mtime_nsec()).hash(&mut h);
    }
    Some(format!("{:016x}", h.finish()))
}

fn menu(paths: &[String], background: bool, client: &mut Option<Client>) -> Vec<Value> {
    if background || paths.is_empty() {
        return Vec::new();
    }
    let (Some(kinds), Some(context)) = (kinds(paths), identity(paths)) else { return Vec::new() };
    let mut actions = vec![json!({ "id": SHEET, "label": "Share…", "icon": "share", "context": context })];
    let kinds = nyx_share_core::kinds_of(&kinds);
    if let Ok(client) = connected(client)
        && let Ok(dests) = client.destinations(&kinds, paths.len())
    {
        for (i, d) in dests.iter().filter(|d| d.available && !d.hidden && !d.needs_folder).take(16).enumerate() {
            actions.push(json!({
                "id": format!("dest-{i}"),
                "label": d.name,
                "placement": "send-to",
                "context": format!("{context}:{}", d.id),
            }));
        }
    }
    actions
}

fn activate(paths: &[String], action: &str, context: &str, client: &mut Option<Client>) -> Value {
    let (expected, destination) = match context.split_once(':') {
        Some((identity, dest)) => (identity, Some(dest)),
        None => (context, None),
    };
    if identity(paths).as_deref() != Some(expected) || (action == SHEET) == destination.is_some() {
        return rejected(paths.len(), "These files changed since the menu opened. Open the menu again.");
    }
    let mut content = Content::new();
    for p in paths {
        if let Err(e) = content.file(Path::new(p)) {
            return rejected(paths.len(), &e.to_string());
        }
    }
    let client = match connected(client) {
        Ok(c) => c,
        Err(e) => return rejected(paths.len(), &e.to_string()),
    };
    let options = ShareOptions { app_name: Some("Files".into()), ..Default::default() };
    let total = paths.len();
    let Some(destination) = destination else {
        return match client.share(&content, &options) {
            Ok(_) => json!({
                "message": "Opened the Share Sheet.",
                "outcome": { "status": "accepted", "accepted": total, "total": total },
                "feedback": "none",
            }),
            Err(e) => rejected(total, &e.to_string()),
        };
    };
    // Files shows a quick result itself and doesn't follow a job, so the service
    // notifies only for one still running when this reply goes back. A second
    // short of QUICK: a job ending right at the edge notifies twice, never zero times.
    let quiet_for = Some(QUICK.as_secs().saturating_sub(1) as u32);
    let job =
        match client.share_to(destination, &content, &ShareOptions { notify: Some(true), notify_after: quiet_for, ..options }) {
            Ok(id) => id,
            Err(e) => return rejected(paths.len(), &e.to_string()),
        };
    let started = Instant::now();
    let finished = loop {
        match client.job(&job) {
            Ok(j) if j.state.finished() => break Some(j),
            Ok(_) if started.elapsed() < QUICK => std::thread::sleep(Duration::from_millis(50)),
            _ => break None,
        }
    };
    match finished {
        Some(j) if j.state == JobState::Completed => json!({
            "message": j.message.unwrap_or_else(|| "Done.".into()),
            "outcome": { "status": "accepted", "accepted": total, "total": total, "job": job },
            "feedback": "notice",
        }),
        Some(j) => rejected(total, j.message.as_deref().unwrap_or("The transfer stopped.")),
        None => json!({
            "message": "Sending continues in the background.",
            "outcome": { "status": "accepted", "accepted": total, "total": total, "job": job },
            "feedback": "notice",
        }),
    }
}

fn rejected(total: usize, message: &str) -> Value {
    json!({ "message": message, "outcome": { "status": "rejected", "accepted": 0, "total": total } })
}
