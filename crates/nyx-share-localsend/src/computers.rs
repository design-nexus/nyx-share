//! My computers: LocalSend devices the user saved, each its own destination,
//! reached at a saved LAN or Tailscale address and always pinned to its key.

use crate::{Nearby, PORT, sender, tailnet};
use nyx_share_core::{ContentKind, Destination, Section};
use nyx_share_providers::{Outcome, Provider, SendError, Task};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const PREFIX: &str = "computer:";
/// A computer reached this recently counts as online.
const FRESH: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Route {
    /// The local network first, then Tailscale.
    #[default]
    Auto,
    Lan,
    Tailscale,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Computer {
    pub id: String,
    pub name: String,
    /// Its LocalSend certificate fingerprint: who it is, whatever its address.
    pub fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lan: Option<String>,
    /// A Tailscale IP or MagicDNS name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tailscale: Option<String>,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub prefer: Route,
    /// Files from it are received without asking (it must prove its key).
    #[serde(default)]
    pub trusted: bool,
    /// Where files from it go; the receive folder if unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receive_folder: Option<PathBuf>,
}

fn default_port() -> u16 {
    PORT
}

impl Computer {
    /// Addresses to try, in order.
    fn candidates(&self) -> Vec<(&str, bool)> {
        let lan = self.lan.as_deref().map(|a| (a, false));
        let ts = self.tailscale.as_deref().map(|a| (a, true));
        match self.prefer {
            Route::Auto | Route::Lan => [lan, ts].into_iter().flatten().collect(),
            Route::Tailscale => [ts, lan].into_iter().flatten().collect(),
        }
    }
}

/// How a computer was last reached.
#[derive(Clone, Debug)]
pub struct Reach {
    pub addr: IpAddr,
    pub port: u16,
    pub tailscale: bool,
    pub at: Instant,
}

/// The computer's state for lists: online (and how), offline, or not checked yet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Presence {
    Unknown,
    Lan,
    Tailscale,
    Offline,
}

fn resolve(host: &str, port: u16) -> Vec<IpAddr> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return vec![ip];
    }
    (host, port).to_socket_addrs().map(|a| a.map(|s| s.ip()).collect()).unwrap_or_default()
}

impl Nearby {
    pub fn computers(&self) -> Vec<Computer> {
        self.computers.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_computers(&self, list: Vec<Computer>) {
        *self.computers.write().unwrap_or_else(|e| e.into_inner()) = list;
        self.events.peers_changed();
    }

    pub fn computer_with_key(&self, fingerprint: &str) -> Option<Computer> {
        self.computers().into_iter().find(|c| c.fingerprint.eq_ignore_ascii_case(fingerprint))
    }

    pub fn presence(&self, id: &str) -> Presence {
        let reach = self.reach.lock().unwrap_or_else(|e| e.into_inner());
        match reach.get(id) {
            Some(Some(r)) if r.at.elapsed() < FRESH => {
                if r.tailscale {
                    Presence::Tailscale
                } else {
                    Presence::Lan
                }
            }
            Some(_) => Presence::Offline,
            None => Presence::Unknown,
        }
    }

    /// A device heard on the network that is a saved computer: it's online there.
    pub(crate) fn note_reach(&self, fingerprint: &str, addr: IpAddr, port: u16) {
        if let Some(c) = self.computer_with_key(fingerprint) {
            let r = Reach { addr, port, tailscale: tailnet::is_tailscale(&addr), at: Instant::now() };
            self.reach.lock().unwrap_or_else(|e| e.into_inner()).insert(c.id, Some(r));
        }
    }

    /// Try a computer's addresses in order, pinned to its key.
    pub fn reach_computer(&self, c: &Computer) -> Option<Reach> {
        for (host, tailscale) in c.candidates() {
            for addr in resolve(host, c.port) {
                let target = sender::Target { fingerprint: c.fingerprint.clone(), addr, port: c.port };
                if sender::register(&self.identity, &self.me(), &target).is_ok() {
                    let r =
                        Reach { addr, port: c.port, tailscale: tailscale || tailnet::is_tailscale(&addr), at: Instant::now() };
                    self.reach.lock().unwrap_or_else(|e| e.into_inner()).insert(c.id.clone(), Some(r.clone()));
                    return Some(r);
                }
            }
        }
        self.reach.lock().unwrap_or_else(|e| e.into_inner()).insert(c.id.clone(), None);
        None
    }

    /// Check every saved computer, each on its own thread.
    pub fn probe_computers(self: &Arc<Self>) {
        for c in self.computers() {
            let n = self.clone();
            std::thread::spawn(move || {
                let before = n.presence(&c.id);
                n.reach_computer(&c);
                if n.presence(&c.id) != before {
                    n.events.peers_changed();
                }
            });
        }
    }

    /// A computer made from a nearby device (`localsend:<fingerprint>`), or met
    /// at an address (IP or name, optionally `:port`) for the first time.
    pub fn new_computer(&self, from: &str) -> Result<Computer, String> {
        let id = uuid::Uuid::new_v4().to_string();
        if let Some(fp) = from.strip_prefix(crate::PREFIX) {
            let peer = self.peer(fp).ok_or("That device isn't nearby any more")?;
            let (lan, tailscale) = if tailnet::is_tailscale(&peer.addr) {
                (None, Some(self.tailnet_name(&peer.addr).unwrap_or_else(|| peer.addr.to_string())))
            } else {
                (Some(peer.addr.to_string()), None)
            };
            return Ok(Computer {
                id,
                name: peer.info.alias,
                fingerprint: fp.to_ascii_uppercase(),
                lan,
                tailscale,
                port: peer.info.port,
                prefer: Route::Auto,
                trusted: false,
                receive_folder: None,
            });
        }
        let (host, port) = match from.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') && p.parse::<u16>().is_ok() => (h, p.parse().unwrap_or(PORT)),
            _ => (from, PORT),
        };
        let host = host.trim().trim_start_matches("https://").trim_end_matches('/');
        let addrs = resolve(host, port);
        if addrs.is_empty() {
            return Err(format!("Couldn't find {host}"));
        }
        let mut last = String::new();
        for addr in addrs {
            let target = sender::Target { fingerprint: String::new(), addr, port };
            match sender::register(&self.identity, &self.me(), &target) {
                Ok(info) => {
                    let ts = tailnet::is_tailscale(&addr);
                    return Ok(Computer {
                        id,
                        name: info.alias,
                        fingerprint: info.fingerprint,
                        lan: (!ts).then(|| host.to_string()),
                        tailscale: ts.then(|| host.to_string()),
                        port,
                        prefer: Route::Auto,
                        trusted: false,
                        receive_folder: None,
                    });
                }
                Err(e) => last = e.to_string(),
            }
        }
        Err(format!("No LocalSend at {host}: {last}"))
    }

    fn tailnet_name(&self, addr: &IpAddr) -> Option<String> {
        match tailnet::status() {
            tailnet::Status::Running { peers, .. } => {
                peers.into_iter().find(|p| IpAddr::V4(p.ip) == *addr).map(|p| p.dns).filter(|d| !d.is_empty())
            }
            _ => None,
        }
    }
}

/// Saved computers as destinations under My devices.
pub struct ComputersProvider(pub Arc<Nearby>);

impl Provider for ComputersProvider {
    fn id(&self) -> &'static str {
        "computers"
    }

    fn name(&self) -> &'static str {
        "My computers"
    }

    fn kinds(&self) -> Vec<ContentKind> {
        vec![ContentKind::Text, ContentKind::Url, ContentKind::Image, ContentKind::File]
    }

    fn destinations(&self) -> Vec<Destination> {
        self.0
            .computers()
            .into_iter()
            .map(|c| {
                let presence = self.0.presence(&c.id);
                Destination {
                    id: format!("{PREFIX}{}", c.id),
                    provider: self.id().into(),
                    name: c.name.clone(),
                    icon: "computer-symbolic".into(),
                    tint: "blue".into(),
                    section: Section::Devices,
                    kinds: self.kinds(),
                    max_items: None,
                    needs_folder: false,
                    needs_recipient: false,
                    // Offline is a hint: sending checks again, and says why if it fails.
                    available: true,
                    status: Some(
                        match presence {
                            Presence::Lan => "LAN",
                            Presence::Tailscale => "Tailscale",
                            Presence::Offline => "Offline",
                            Presence::Unknown => "Checking",
                        }
                        .into(),
                    ),
                    pinned: false,
                    hidden: false,
                    default: false,
                }
            })
            .collect()
    }

    fn send(&self, destination: &str, task: &Task<'_>) -> Result<Outcome, SendError> {
        let id = destination.strip_prefix(PREFIX).unwrap_or(destination);
        let c = self
            .0
            .computers()
            .into_iter()
            .find(|c| c.id == id)
            .ok_or_else(|| SendError::failed("That computer isn't saved any more"))?;
        let reach = self.0.reach.lock().unwrap_or_else(|e| e.into_inner()).get(&c.id).cloned().flatten();
        let reach = match reach.filter(|r| r.at.elapsed() < Duration::from_secs(60)) {
            Some(r) => r,
            None => self.0.reach_computer(&c).ok_or_else(|| {
                let via = match (&c.lan, &c.tailscale) {
                    (Some(_), Some(_)) => "on the local network or over Tailscale",
                    (None, Some(_)) => "over Tailscale",
                    _ => "on the local network",
                };
                SendError::failed(format!("{} is offline: it isn't reachable {via}", c.name))
            })?,
        };
        let target = sender::Target { fingerprint: c.fingerprint.clone(), addr: reach.addr, port: reach.port };
        crate::deliver(&self.0, &target, &c.name, task)
    }
}
