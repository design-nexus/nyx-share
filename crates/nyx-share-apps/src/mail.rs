//! A new message in Mail with the shared text, links and files, through Mail's
//! `mail.draft` tool. Mail opens it for the user to address and send; nothing
//! is sent from here.

use crate::control::{self, App};
use nyx_share_core::{ContentKind, Destination, Section};
use nyx_share_providers::stage::Stage;
use nyx_share_providers::{Data, Outcome, Provider, SendError, Task};
use serde_json::json;
use std::path::PathBuf;

/// Mail refuses attachments over this, as most mail servers do.
const MAX_FILE: u64 = 25_000_000;
const MAX_FILES: u32 = 20;

pub struct Mail {
    app: App,
    /// Where files wait while Mail reads them: `$XDG_CACHE_HOME/nyx-share/mail`,
    /// inside the home folder as Mail requires.
    stage: PathBuf,
}

impl Default for Mail {
    fn default() -> Self {
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache"));
        Self {
            app: App::new("Mail", "io.github.design_nexus.nyx.Mail", "nyx-mail", "NYX_SHARE_MAIL_ID"),
            stage: cache.join("nyx-share/mail"),
        }
    }
}

impl Provider for Mail {
    fn id(&self) -> &'static str {
        "mail"
    }

    fn name(&self) -> &'static str {
        "Mail"
    }

    fn kinds(&self) -> Vec<ContentKind> {
        vec![ContentKind::Text, ContentKind::Url, ContentKind::Image, ContentKind::File]
    }

    fn destinations(&self) -> Vec<Destination> {
        if !self.app.installed() {
            return Vec::new();
        }
        vec![Destination {
            id: "mail".into(),
            provider: self.id().into(),
            name: "Mail".into(),
            icon: "mail-send-symbolic".into(),
            tint: "blue".into(),
            section: Section::Apps,
            kinds: self.kinds(),
            max_items: Some(MAX_FILES),
            needs_folder: false,
            available: true,
            status: None,
            pinned: false,
            hidden: false,
            default: false,
        }]
    }

    fn send(&self, _destination: &str, task: &Task<'_>) -> Result<Outcome, SendError> {
        let failed = SendError::failed;
        if let Some(big) = task.items.iter().find(|i| matches!(i.data, Data::File(_)) && i.info.size > MAX_FILE) {
            return Err(failed(format!("{} is over 25 MB, which most mail servers refuse", big.info.name)));
        }
        let texts: Vec<&str> =
            task.items.iter().filter_map(|i| if let Data::Text(t) = &i.data { Some(t.as_str()) } else { None }).collect();
        let body = texts.join("\n\n");
        let subject = match task.items {
            [one] if one.info.kind == ContentKind::Url => String::new(),
            [one] if one.info.kind == ContentKind::Text => crate::notes::title_for(&body),
            [one] => one.info.name.clone(),
            _ => String::new(),
        };
        let mut stage = Stage::new(&self.stage).map_err(|e| failed(format!("Couldn't prepare the files: {e}")))?;
        let mut paths = Vec::new();
        for item in task.items {
            if task.cancelled() {
                return Err(SendError::Cancelled { outputs: Vec::new() });
            }
            if let Data::File(file) = &item.data {
                paths.push(stage.place(file, &item.info.name, item.info.size, task)?.display().to_string());
            }
        }
        let bus = control::session().map_err(failed)?;
        (task.step)(Some("Opening Mail…"));
        self.app.ensure(&bus).map_err(failed)?;
        let args = json!({ "subject": subject, "body": body, "attachments": paths });
        // Mail reads the files during the call, so the stage can go right after.
        self.app.call(&bus, "mail.draft", &args).map_err(failed)?;
        drop(stage);
        (task.progress)(task.items.iter().map(|i| i.size()).sum());
        let message = match paths.len() {
            0 => "Opened a message in Mail for you to address and send".into(),
            1 => "Opened a message in Mail with the file attached".into(),
            n => format!("Opened a message in Mail with {n} files attached"),
        };
        Ok(Outcome { message, outputs: Vec::new() })
    }
}
