//! A note from shared text or a link, through Notes' own tools: Notes shows the
//! new note for the user to accept, and Share reports what they decided.

use crate::control::{self, App};
use nyx_share_core::{ContentKind, Destination, Section};
use nyx_share_providers::{Data, Outcome, Provider, SendError, Task};
use serde_json::json;
use std::time::{Duration, Instant};

/// Notes takes at most this much text in one proposal.
const MAX_TEXT: usize = 128 * 1024;
/// How long Share waits for the user to decide in Notes.
const REVIEW: Duration = Duration::from_secs(15 * 60);

pub struct Notes(App);

impl Default for Notes {
    fn default() -> Self {
        Self(App::new("Notes", "io.github.design_nexus.nyx.Notes", "nyx-notes", "NYX_SHARE_NOTES_ID"))
    }
}

/// A title from the start of the text: its first line, at most 60 characters.
pub fn title_for(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("Shared text");
    let line = line.trim_start_matches('#').trim();
    match line.char_indices().nth(60) {
        Some((cut, _)) => format!("{}…", line[..cut].trim_end()),
        None => line.to_string(),
    }
}

fn host(url: &str) -> String {
    url.split_once("://").map(|(_, rest)| rest).unwrap_or(url).split(['/', '?', '#']).next().unwrap_or(url).to_string()
}

impl Provider for Notes {
    fn id(&self) -> &'static str {
        "notes"
    }

    fn name(&self) -> &'static str {
        "Notes"
    }

    fn kinds(&self) -> Vec<ContentKind> {
        vec![ContentKind::Text, ContentKind::Url]
    }

    fn destinations(&self) -> Vec<Destination> {
        if !self.0.installed() {
            return Vec::new();
        }
        vec![Destination {
            id: "notes".into(),
            provider: self.id().into(),
            name: "Notes".into(),
            icon: "accessories-text-editor-symbolic".into(),
            tint: "amber".into(),
            section: Section::Apps,
            kinds: self.kinds(),
            max_items: Some(1),
            needs_folder: false,
            needs_recipient: false,
            available: true,
            status: None,
            pinned: false,
            hidden: false,
            default: false,
        }]
    }

    fn send(&self, _destination: &str, task: &Task<'_>) -> Result<Outcome, SendError> {
        let [item] = task.items else { return Err(SendError::failed("Notes takes one text or link at a time")) };
        let Data::Text(text) = &item.data else { return Err(SendError::failed("Notes takes text and links")) };
        if text.len() > MAX_TEXT {
            return Err(SendError::failed("Too long for one note: Notes takes up to 128 KiB"));
        }
        let failed = SendError::failed;
        let bus = control::session().map_err(failed)?;
        (task.step)(Some("Opening Notes…"));
        self.0.ensure(&bus).map_err(failed)?;
        let web = item.info.kind == ContentKind::Url && (text.starts_with("https://") || text.starts_with("http://"));
        let (tool, args) = match web {
            true => ("notes.clip", json!({ "title": host(text), "url": text, "text": format!("<{text}>") })),
            false => ("notes.vault_create", json!({ "title": title_for(text), "text": text })),
        };
        let proposal = self.0.call(&bus, tool, &args).map_err(failed)?;
        (task.progress)(text.len() as u64);
        let Some(id) = proposal.get("proposal_id").and_then(|v| v.as_str()).map(String::from) else {
            return Ok(Outcome { message: "Notes shows the note for you to check".into(), outputs: Vec::new() });
        };
        (task.step)(Some("Waiting for you to check it in Notes…"));
        let start = Instant::now();
        while start.elapsed() < REVIEW {
            if task.cancelled() {
                return Err(SendError::Cancelled { outputs: Vec::new() });
            }
            std::thread::sleep(Duration::from_secs(1));
            let status = match self.0.call(&bus, "notes.vault_proposal_status", &json!({ "proposal_id": id })) {
                Ok(s) => s,
                // Notes closed: the proposal went with it.
                Err(_) if !self.0.running(&bus) => return Err(SendError::failed("Notes closed before the note was added")),
                Err(e) => return Err(failed(e)),
            };
            match status.get("status").and_then(|s| s.as_str()) {
                Some("applied") => return Ok(Outcome { message: "Added to Notes".into(), outputs: Vec::new() }),
                Some("not_applied") => {
                    let why = status.get("error").and_then(|e| e.as_str()).unwrap_or("you didn't add it");
                    return Err(failed(format!("Notes didn't add it: {why}")));
                }
                _ => {}
            }
        }
        Ok(Outcome { message: "Notes still shows the note for you to check".into(), outputs: Vec::new() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_come_from_the_first_line() {
        assert_eq!(title_for("\n\n# Plans for Monday\nmore"), "Plans for Monday");
        assert_eq!(title_for(""), "Shared text");
        let long = title_for(&"word ".repeat(30));
        assert!(long.ends_with('…') && long.chars().count() <= 61, "{long}");
        assert_eq!(host("https://example.com/a?b"), "example.com");
    }
}
