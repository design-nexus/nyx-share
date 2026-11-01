//! Recent Telegram chats from Nyx Pulse, through its control tools: a text or a
//! link goes to the chosen chat with `pulse.messages.send`. Pulse must be open
//! and signed in; Share doesn't start it just to list chats.

use crate::control::{self, App};
use nyx_share_core::{ContentKind, Destination, Section};
use nyx_share_providers::{Data, Outcome, Provider, SendError, Task};
use serde_json::{Value, json};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const PREFIX: &str = "pulse:";
/// How many recent chats the Share Sheet offers.
const CHATS: usize = 6;
/// The chat list is reused this long: it is asked for on every sheet.
const FRESH: Duration = Duration::from_secs(10);
/// Telegram's limit for one message.
const MAX_TEXT: usize = 4096;

#[derive(Clone, Debug)]
pub struct Chat {
    pub id: i64,
    pub title: String,
}

pub struct Pulse {
    app: App,
    cache: Mutex<Option<(Instant, Vec<Chat>)>>,
}

impl Default for Pulse {
    fn default() -> Self {
        Self {
            app: App::new("Pulse", "io.github.design_nexus.nyx.Pulse", "nyx-pulse", "NYX_SHARE_PULSE_ID"),
            cache: Mutex::new(None),
        }
    }
}

fn chats_from(value: &Value) -> Vec<Chat> {
    value["chats"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| Some(Chat { id: c["id"].as_i64()?, title: c["title"].as_str()?.trim().to_string() }))
        // A chat without a name can't be told apart in the sheet.
        .filter(|c| !c.title.is_empty())
        .collect()
}

impl Pulse {
    pub fn chats(&self) -> Vec<Chat> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, list)) = cache.as_ref()
            && at.elapsed() < FRESH
        {
            return list.clone();
        }
        let list = control::session()
            .ok()
            .filter(|bus| self.app.running(bus))
            .and_then(|bus| self.app.call(&bus, "pulse.chats.list", &json!({ "limit": CHATS })).ok())
            .map(|v| chats_from(&v))
            .unwrap_or_default();
        *cache = Some((Instant::now(), list.clone()));
        list
    }
}

impl Provider for Pulse {
    fn id(&self) -> &'static str {
        "pulse"
    }

    fn name(&self) -> &'static str {
        "Telegram"
    }

    fn kinds(&self) -> Vec<ContentKind> {
        vec![ContentKind::Text, ContentKind::Url]
    }

    fn destinations(&self) -> Vec<Destination> {
        self.chats()
            .into_iter()
            .map(|c| Destination {
                id: format!("{PREFIX}{}", c.id),
                provider: self.id().into(),
                name: c.title,
                icon: "user-available-symbolic".into(),
                tint: "teal".into(),
                section: Section::Apps,
                kinds: self.kinds(),
                max_items: Some(1),
                needs_folder: false,
                available: true,
                status: Some("Telegram".into()),
                pinned: false,
                hidden: false,
                default: false,
            })
            .collect()
    }

    fn send(&self, destination: &str, task: &Task<'_>) -> Result<Outcome, SendError> {
        let chat: i64 = destination
            .strip_prefix(PREFIX)
            .and_then(|id| id.parse().ok())
            .ok_or_else(|| SendError::failed("That isn't a Telegram chat"))?;
        let [item] = task.items else { return Err(SendError::failed("Telegram takes one text or link at a time")) };
        let Data::Text(text) = &item.data else { return Err(SendError::failed("Telegram takes text and links here")) };
        if text.chars().count() > MAX_TEXT {
            return Err(SendError::failed("Too long for one Telegram message (4096 characters)"));
        }
        let bus = control::session().map_err(SendError::failed)?;
        if !self.app.running(&bus) {
            return Err(SendError::failed("Open Pulse to send to Telegram"));
        }
        let title = self.chats().into_iter().find(|c| c.id == chat).map(|c| c.title).unwrap_or_else(|| "the chat".into());
        let reply =
            self.app.call(&bus, "pulse.messages.send", &json!({ "chat_id": chat, "text": text })).map_err(SendError::failed)?;
        if reply["sent"].as_bool() != Some(true) {
            return Err(SendError::failed("Pulse isn't connected to Telegram"));
        }
        (task.progress)(text.len() as u64);
        Ok(Outcome { message: format!("Sent to {title} on Telegram"), outputs: Vec::new() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_chat_list() {
        let v =
            json!({ "chats": [{ "id": 42, "title": "Saved Messages" }, { "id": "bad" }, { "id": 7, "title": " " }], "count": 3 });
        let chats = chats_from(&v);
        assert_eq!(chats.len(), 1);
        assert_eq!((chats[0].id, chats[0].title.as_str()), (42, "Saved Messages"));
    }
}
