//! Nearby sharing (LocalSend) inside the service: settings, startup, and the
//! bridge that turns offers into Share Sheet prompts and receive jobs.

use crate::launch;
use crate::state::{Answer, Incoming, State, lock};
use nyx_share_localsend::computers::{Computer, ComputersProvider};
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
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub visibility: Visibility,
    /// The name other devices see; the host name if unset.
    pub alias: Option<String>,
    /// Where received files go; the XDG Downloads folder if unset.
    pub receive_folder: Option<PathBuf>,
    /// Also look for LocalSend on the devices of the user's tailnet.
    pub tailscale: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { visibility: Visibility::Running, alias: None, receive_folder: None, tailscale: true }
    }
}

/// Look for devices: multicast on the local network, the tailnet, and the
/// saved computers at their addresses.
pub fn look(nearby: &Arc<Nearby>) {
    nearby.scan();
    nearby.probe_tailnet();
    nearby.probe_computers();
}

#[derive(Default, Serialize, Deserialize)]
struct ComputersFile {
    #[serde(default, rename = "computer")]
    computers: Vec<Computer>,
}

fn computers_path() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("nyx-share/computers.toml")
}

pub fn load_computers() -> Vec<Computer> {
    std::fs::read_to_string(computers_path())
        .ok()
        .and_then(|s| toml::from_str::<ComputersFile>(&s).ok())
        .map(|f| f.computers)
        .unwrap_or_default()
}

pub fn save_computers(state: &State, computers: Vec<Computer>) -> std::io::Result<()> {
    let path = computers_path();
    std::fs::create_dir_all(path.parent().expect("has a parent"))?;
    let text = toml::to_string(&ComputersFile { computers: computers.clone() }).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, &path)?;
    if let Some(n) = state.nearby() {
        n.set_computers(computers);
        n.probe_computers();
    }
    state.destinations_changed();
    Ok(())
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home().join(fallback))
}

pub fn settings() -> Settings {
    let path = config_path();
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

fn config_path() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("nyx-share/nearby.toml")
}

fn identity_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share").join("nyx-share/localsend")
}

pub fn alias(settings: &Settings) -> String {
    settings.alias.clone().filter(|a| !a.trim().is_empty()).unwrap_or_else(host_name)
}

static PROBLEM: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn problem() -> Option<String> {
    PROBLEM.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The fingerprint of this device's key, if it has one yet.
pub fn fingerprint() -> String {
    std::fs::read(identity_dir().join("cert.der")).map(|der| nyx_share_localsend::identity::fingerprint(&der)).unwrap_or_default()
}

/// Start Nearby unless it's turned off. Runs on its own thread: the first start
/// makes a key pair, which takes a moment.
pub fn start(state: Arc<State>) {
    let settings = settings();
    if settings.visibility == Visibility::Off || state.nearby().is_some() {
        return;
    }
    let bridge = Arc::new(Bridge(Arc::downgrade(&state)));
    match Nearby::start(&identity_dir(), alias(&settings), settings.tailscale, bridge) {
        Ok(nearby) => {
            *PROBLEM.lock().unwrap_or_else(|e| e.into_inner()) = None;
            nearby.set_computers(load_computers());
            state.set_nearby(Some(nearby.clone()));
            state.add_provider(Arc::new(ComputersProvider(nearby.clone())));
            state.add_provider(Arc::new(NearbyProvider(nearby.clone())));
            nearby.probe_computers();
        }
        Err(e) => {
            eprintln!("nyx-share: nearby sharing is off: {e}");
            *PROBLEM.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("Nearby sharing couldn't start: {e}"));
        }
    }
    state.destinations_changed();
    state.nearby_changed();
}

pub fn stop(state: &State) {
    if let Some(n) = state.nearby() {
        n.stop();
    }
    state.remove_provider("localsend");
    state.remove_provider("computers");
    state.set_nearby(None);
    state.destinations_changed();
    state.nearby_changed();
}

/// Save the fields present and apply them: a new name restarts Nearby.
pub fn update(state: &Arc<State>, update: nyx_share_dbus::NearbyUpdate) -> std::io::Result<()> {
    let mut s = settings();
    let old_alias = alias(&s);
    let old_visibility = s.visibility.clone();
    let old_tailscale = s.tailscale;
    if let Some(t) = update.tailscale {
        s.tailscale = t;
    }
    if let Some(v) = update.visibility {
        s.visibility = match v.as_str() {
            "off" => Visibility::Off,
            "running" => Visibility::Running,
            _ => return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "visibility is running or off")),
        };
    }
    if let Some(a) = update.alias {
        let a: String = a.chars().filter(|c| !c.is_control()).take(64).collect();
        s.alias = Some(a.trim().to_string()).filter(|a| !a.is_empty());
    }
    if let Some(f) = update.receive_folder {
        let f = PathBuf::from(f);
        if !f.is_absolute() || !f.is_dir() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "the receive folder must be an existing folder"));
        }
        s.receive_folder = Some(f);
    }
    let path = config_path();
    std::fs::create_dir_all(path.parent().expect("has a parent"))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, toml::to_string(&s).map_err(std::io::Error::other)?)?;
    std::fs::rename(tmp, &path)?;
    let restart = s.visibility == Visibility::Running
        && (old_visibility == Visibility::Off || alias(&s) != old_alias || s.tailscale != old_tailscale);
    if s.visibility == Visibility::Off || restart {
        stop(state);
    }
    if s.visibility == Visibility::Running && state.nearby().is_none() {
        let st = state.clone();
        std::thread::spawn(move || start(st));
    }
    state.nearby_changed();
    Ok(())
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
        // A trusted computer that proved its key is received from without asking.
        let trusted = offer.verified
            && state.nearby().and_then(|n| n.computer_with_key(&offer.sender.fingerprint)).is_some_and(|c| c.trusted);
        if trusted && !offer.message_only {
            let incoming = Incoming {
                token,
                offer: offer.clone(),
                answer: Answer::Accepted,
                dismissed: true,
                folder: self.receive_folder(offer).display().to_string(),
            };
            lock(&state.incoming).insert(offer.session.clone(), incoming);
            return Decision::Accept;
        }
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

    fn receive_folder(&self, offer: &Offer) -> PathBuf {
        let own = self
            .0
            .upgrade()
            .and_then(|s| s.nearby())
            .and_then(|n| n.computer_with_key(&offer.sender.fingerprint))
            .and_then(|c| c.receive_folder)
            .filter(|p| p.is_absolute());
        let folder = own.unwrap_or_else(receive_folder);
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
