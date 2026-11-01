use crate::{Data, Outcome, Provider, SendError, Task, process};
use nyx_share_core::{ContentKind, Destination, Section};
use std::io::Write;
use std::process::Stdio;

/// The desktop clipboard, through `wl-copy`, which keeps serving the content
/// after the service exits.
pub struct Clipboard;

impl Clipboard {
    fn unavailable() -> Option<String> {
        if std::env::var_os("WAYLAND_DISPLAY").is_none() {
            return Some("No Wayland session".into());
        }
        process::find("wl-copy").is_none().then(|| "Install wl-clipboard to copy".into())
    }
}

impl Provider for Clipboard {
    fn id(&self) -> &'static str {
        "clipboard"
    }

    fn name(&self) -> &'static str {
        "Clipboard"
    }

    fn kinds(&self) -> Vec<ContentKind> {
        vec![ContentKind::Text, ContentKind::Url, ContentKind::Image]
    }

    fn destinations(&self) -> Vec<Destination> {
        let problem = Self::unavailable();
        vec![Destination {
            id: "clipboard".into(),
            provider: self.id().into(),
            name: "Copy".into(),
            icon: "edit-copy-symbolic".into(),
            tint: "graphite".into(),
            section: Section::Apps,
            kinds: self.kinds(),
            max_items: Some(1),
            needs_folder: false,
            needs_recipient: false,
            available: problem.is_none(),
            status: problem,
            pinned: false,
            hidden: false,
            default: false,
        }]
    }

    fn send(&self, _destination: &str, task: &Task<'_>) -> Result<Outcome, SendError> {
        if let Some(problem) = Self::unavailable() {
            return Err(SendError::failed(problem));
        }
        let [item] = task.items else {
            return Err(SendError::failed("Only one item can be copied at a time"));
        };
        let mime = match item.info.kind {
            ContentKind::Text | ContentKind::Url => "text/plain;charset=utf-8".to_string(),
            ContentKind::Image => item.info.mime.clone().unwrap_or_else(|| "image/png".into()),
            ContentKind::File => return Err(SendError::failed("Files can't be copied to the clipboard")),
        };
        let program = process::find("wl-copy").ok_or_else(|| SendError::failed("Install wl-clipboard to copy"))?;
        let mut child = process::detached(&program)
            .args(["--type", &mime])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| SendError::failed(format!("Couldn't start wl-copy: {e}")))?;
        let mut stdin = child.stdin.take().expect("piped");
        let written = match &item.data {
            Data::Text(text) => {
                let text = if item.info.kind == ContentKind::Url { text.trim() } else { text };
                let r = stdin.write_all(text.as_bytes()).map_err(|e| SendError::failed(format!("Couldn't copy: {e}")));
                (task.progress)(text.len() as u64);
                r
            }
            Data::File(file) => crate::copy_file(file, item.info.size, &mut stdin, task),
        };
        drop(stdin);
        let status = child.wait().map_err(|e| SendError::failed(format!("wl-copy failed: {e}")))?;
        written?;
        if !status.success() {
            return Err(SendError::failed("wl-copy couldn't take the clipboard"));
        }
        let what = match item.info.kind {
            ContentKind::Url => "Link copied",
            ContentKind::Image => "Image copied",
            _ => "Text copied",
        };
        Ok(Outcome { message: what.into(), outputs: Vec::new() })
    }
}
