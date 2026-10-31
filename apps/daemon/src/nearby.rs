//! Nearby sharing (LocalSend) inside the service: settings, startup, and the
//! bridge that turns offers into Share Sheet prompts and receive jobs.

use crate::launch;
use crate::state::{Answer, Incoming, State, lock};
use nyx_share_localsend::{Decision, End, Events, Nearby, NearbyProvider, Offer};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

/// How long an offer waits for an answer before it is declined.
const ANSWER_WITHIN: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    /// Discoverable and receiving while the service runs.
    #[default]
    Running,
    Off,
}

/// `$XDG_CONFIG_HOME/nyx-share/nearby.toml`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub visibility: Visibility,
    /// The name other devices see; the host name if unset.
    pub alias: Option<String>,
    /// Where received files go; the XDG Downloads folder if unset.
    pub receive_folder: Option<PathBuf>,
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home().join(fallback))
}

pub fn settings() -> Settings {
    let path = xdg("XDG_CONFIG_HOME", ".config").join("nyx-share/nearby.toml");
    std::fs::read_to_string(&path).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
}

fn host_name() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for its length.
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    let name = if ok {
        String::from_utf8_lossy(&buf[..buf.iter().position(|&b| b == 0).unwrap_or(0)]).into_owned()
    } else {
        String::new()
    };
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Linux computer".into(),
    }
}

/// XDG_DOWNLOAD_DIR from user-dirs.dirs, else ~/Downloads.
fn downloads() -> PathBuf {
    let dirs = xdg("XDG_CONFIG_HOME", ".config").join("user-dirs.dirs");
    let configured = std::fs::read_to_string(dirs).ok().and_then(|text| {
        let line = text.lines().find(|l| l.trim_start().starts_with("XDG_DOWNLOAD_DIR="))?;
        let value = line.split_once('=')?.1.trim().trim_matches('"');
        Some(PathBuf::from(value.replace("$HOME", &home().display().to_string())))
    });
    configured.filter(|p| p.is_absolute()).unwrap_or_else(|| home().join("Downloads"))
}

pub fn receive_folder() -> PathBuf {
    settings().receive_folder.filter(|p| p.is_absolute()).unwrap_or_else(downloads)
}

/// Start Nearby unless it's turned off. Runs on its own thread: the first start
/// makes a key pair, which takes a moment.
pub fn start(state: Arc<State>) {
    let settings = settings();
    if settings.visibility == Visibility::Off {
        return;
    }
    let dir = xdg("XDG_DATA_HOME", ".local/share").join("nyx-share/localsend");
    let alias = settings.alias.filter(|a| !a.trim().is_empty()).unwrap_or_else(host_name);
    let bridge = Arc::new(Bridge(Arc::downgrade(&state)));
    match Nearby::start(&dir, alias, bridge) {
        Ok(nearby) => {
            let _ = state.nearby.set(nearby.clone());
            state.add_provider(Arc::new(NearbyProvider(nearby)));
            state.destinations_changed();
        }
        Err(e) => eprintln!("nyx-share: nearby sharing is off: {e}"),
    }
}

struct Bridge(Weak<State>);

impl Events for Bridge {
    fn peers_changed(&self) {
        if let Some(s) = self.0.upgrade() {
            s.destinations_changed();
        }
    }

    fn offer(&self, offer: &Offer) -> Decision {
        let Some(state) = self.0.upgrade() else { return Decision::Decline };
        state.touch();
        let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        let incoming = Incoming {
            token: token.clone(),
            offer: offer.clone(),
            answer: if offer.message_only { Answer::Accepted } else { Answer::Pending },
            dismissed: false,
            folder: receive_folder().display().to_string(),
        };
        lock(&state.incoming).insert(offer.session.clone(), incoming);
        if let Err(e) = launch::incoming(&offer.session, &token) {
            lock(&state.incoming).remove(&offer.session);
            state.notify_text(&format!("{} wants to send you files", offer.sender.alias), &e);
            return Decision::Decline;
        }
        if offer.message_only {
            return Decision::Accept;
        }
        let deadline = Instant::now() + ANSWER_WITHIN;
        let mut guard = lock(&state.incoming);
        loop {
            match guard.get(&offer.session).map(|i| i.answer) {
                Some(Answer::Accepted) => return Decision::Accept,
                Some(Answer::Pending) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        if let Some(i) = guard.get_mut(&offer.session) {
                            i.answer = Answer::Expired;
                        }
                        return Decision::Decline;
                    }
                    guard = state.answered.wait_timeout(guard, left).unwrap_or_else(|e| e.into_inner()).0;
                }
                _ => return Decision::Decline,
            }
        }
    }

    fn refused(&self, offer: &Offer, reason: &str) {
        if let Some(s) = self.0.upgrade() {
            s.notify_text(&format!("Couldn't receive from {}", offer.sender.alias), reason);
        }
    }

    fn receive_folder(&self) -> PathBuf {
        let folder = receive_folder();
        let _ = std::fs::create_dir_all(&folder);
        folder
    }

    fn started(&self, offer: &Offer) {
        if let Some(s) = self.0.upgrade() {
            s.begin_receive(offer);
        }
    }

    fn progress(&self, session: &str, bytes: u64) {
        if let Some(s) = self.0.upgrade() {
            s.receive_progress(session, bytes);
        }
    }

    fn saved(&self, session: &str, path: PathBuf) {
        if let Some(s) = self.0.upgrade() {
            s.received(session, path.display().to_string());
        }
    }

    fn ended(&self, session: &str, end: End) {
        if let Some(s) = self.0.upgrade() {
            s.end_receive(session, end);
        }
    }
}
