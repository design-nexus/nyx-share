//! LocalSend protocol v2 for Nyx Share: discovery, sending and receiving,
//! compatible with the official LocalSend apps. See docs/LOCALSEND.md.

pub mod computers;
pub mod discovery;
pub mod dto;
pub mod http;
pub mod identity;
pub mod receiver;
pub mod sender;
pub mod tailnet;
pub mod tls;

pub use dto::DeviceInfo;
pub use receiver::{Decision, End, Offer};

use nyx_share_core::{ContentKind, Destination, Section};
use nyx_share_providers::{Data, Outcome, Provider, SendError, Task};
use std::collections::HashMap;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

pub const PORT: u16 = 53317;
pub const PROTOCOL_VERSION: &str = "2.1";
/// A device not heard from for this long leaves the list.
const FORGET: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug)]
pub struct Peer {
    pub info: DeviceInfo,
    pub addr: IpAddr,
    /// It answered over TLS with the certificate it announced.
    pub verified: bool,
    pub seen: Instant,
}

/// What Nearby needs from the service: the person's answers and the job display.
pub trait Events: Send + Sync {
    fn peers_changed(&self);
    fn offer(&self, offer: &Offer) -> Decision;
    fn refused(&self, offer: &Offer, reason: &str);
    fn receive_folder(&self, offer: &Offer) -> PathBuf;
    fn started(&self, offer: &Offer);
    fn progress(&self, session: &str, bytes: u64);
    fn saved(&self, session: &str, path: PathBuf);
    fn ended(&self, session: &str, end: End);
}

pub struct Nearby {
    identity: identity::Identity,
    me: Mutex<DeviceInfo>,
    peers: Mutex<HashMap<String, Peer>>,
    multicast: Option<discovery::Multicast>,
    receiver: OnceLock<Arc<receiver::Receiver>>,
    events: Arc<dyn Events>,
    stopped: std::sync::atomic::AtomicBool,
    /// Look for LocalSend on tailnet devices too.
    tailscale: bool,
    probing: std::sync::atomic::AtomicBool,
    computers: std::sync::RwLock<Vec<computers::Computer>>,
    /// Computer id to how it was last reached (None: tried and failed).
    reach: Mutex<HashMap<String, Option<computers::Reach>>>,
}

struct Bridge(Weak<Nearby>);

impl receiver::Host for Bridge {
    fn me(&self) -> DeviceInfo {
        self.0.upgrade().map(|n| n.me()).unwrap_or_else(|| DeviceInfo {
            alias: String::new(),
            version: PROTOCOL_VERSION.into(),
            device_model: None,
            device_type: None,
            fingerprint: String::new(),
            port: PORT,
            protocol: "https".into(),
            download: false,
            announce: None,
        })
    }
    fn registered(&self, info: DeviceInfo, addr: IpAddr, verified: bool) {
        if let Some(n) = self.0.upgrade() {
            n.heard(info, addr, verified, false);
        }
    }
    fn offer(&self, offer: &Offer) -> Decision {
        match self.0.upgrade() {
            Some(n) => n.events.offer(offer),
            None => Decision::Decline,
        }
    }
    fn refused(&self, offer: &Offer, reason: &str) {
        if let Some(n) = self.0.upgrade() {
            n.events.refused(offer, reason);
        }
    }
    fn receive_folder(&self, offer: &Offer) -> PathBuf {
        self.0.upgrade().map(|n| n.events.receive_folder(offer)).unwrap_or_default()
    }
    fn started(&self, offer: &Offer) {
        if let Some(n) = self.0.upgrade() {
            n.events.started(offer);
        }
    }
    fn progress(&self, session: &str, bytes: u64) {
        if let Some(n) = self.0.upgrade() {
            n.events.progress(session, bytes);
        }
    }
    fn saved(&self, session: &str, path: PathBuf) {
        if let Some(n) = self.0.upgrade() {
            n.events.saved(session, path);
        }
    }
    fn ended(&self, session: &str, end: End) {
        if let Some(n) = self.0.upgrade() {
            n.events.ended(session, end);
        }
    }
}

impl Nearby {
    /// Start receiving and discovery. `dir` keeps this device's identity.
    pub fn start(dir: &Path, alias: String, tailscale: bool, events: Arc<dyn Events>) -> io::Result<Arc<Self>> {
        let identity = identity::Identity::load_or_create(dir)?;
        let me = DeviceInfo {
            alias,
            version: PROTOCOL_VERSION.into(),
            device_model: Some("Linux".into()),
            device_type: Some("desktop".into()),
            fingerprint: identity.fingerprint.clone(),
            port: PORT,
            protocol: "https".into(),
            download: false,
            announce: None,
        };
        let multicast = discovery::Multicast::open().map_err(|e| eprintln!("nyx-share: no LocalSend discovery: {e}")).ok();
        let nearby = Arc::new(Self {
            identity,
            me: Mutex::new(me),
            peers: Mutex::new(HashMap::new()),
            multicast,
            receiver: OnceLock::new(),
            events,
            stopped: std::sync::atomic::AtomicBool::new(false),
            tailscale,
            probing: std::sync::atomic::AtomicBool::new(false),
            computers: std::sync::RwLock::new(Vec::new()),
            reach: Mutex::new(HashMap::new()),
        });
        let receiver = receiver::Receiver::start(&nearby.identity, Arc::new(Bridge(Arc::downgrade(&nearby))), PORT)?;
        nearby.me.lock().unwrap_or_else(|e| e.into_inner()).port = receiver.port;
        let _ = nearby.receiver.set(receiver);
        let listener = Arc::downgrade(&nearby);
        std::thread::spawn(move || {
            loop {
                let Some(n) = listener.upgrade().filter(|n| !n.stopped.load(std::sync::atomic::Ordering::Relaxed)) else { break };
                let Some(mc) = n.multicast.as_ref().filter(|m| m.listening()) else { break };
                if let Some((info, ip)) = mc.receive() {
                    let announce = info.announce == Some(true);
                    n.heard(info, IpAddr::V4(ip), false, announce);
                }
            }
        });
        let announcer = Arc::downgrade(&nearby);
        std::thread::spawn(move || {
            for wait in [0, 1, 3] {
                std::thread::sleep(Duration::from_secs(wait));
                let Some(n) = announcer.upgrade() else { break };
                n.scan();
            }
        });
        nearby.probe_tailnet();
        Ok(nearby)
    }

    pub fn me(&self) -> DeviceInfo {
        self.me.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn fingerprint(&self) -> &str {
        &self.identity.fingerprint
    }

    pub fn port(&self) -> u16 {
        self.me().port
    }

    pub fn discovering(&self) -> bool {
        self.multicast.as_ref().is_some_and(|m| m.listening())
    }

    /// Ask nearby devices to answer.
    pub fn scan(&self) {
        if self.stopped.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        if let Some(mc) = &self.multicast {
            mc.announce(&self.me(), true);
        }
    }

    /// Introduce ourselves to every online tailnet device that answers on the
    /// LocalSend port. Each check runs on its own thread; one round at a time.
    pub fn probe_tailnet(self: &Arc<Self>) {
        use std::sync::atomic::Ordering;
        if !self.tailscale || self.stopped.load(Ordering::Relaxed) || self.probing.swap(true, Ordering::Relaxed) {
            return;
        }
        let me = self.clone();
        std::thread::spawn(move || {
            if let tailnet::Status::Running { me: own, peers } = tailnet::status() {
                let handles: Vec<_> = peers
                    .into_iter()
                    .filter(|p| p.online && p.ip != own)
                    .map(|p| {
                        let n = me.clone();
                        std::thread::spawn(move || {
                            let target = sender::Target { fingerprint: String::new(), addr: IpAddr::V4(p.ip), port: PORT };
                            if let Ok(info) = sender::register(&n.identity, &n.me(), &target) {
                                n.heard(
                                    DeviceInfo { port: PORT, protocol: "https".into(), ..info },
                                    IpAddr::V4(p.ip),
                                    true,
                                    false,
                                );
                            }
                        })
                    })
                    .collect();
                for h in handles {
                    let _ = h.join();
                }
            }
            me.probing.store(false, Ordering::Relaxed);
        });
    }

    /// Stop receiving and discovery for good; start a new Nearby to resume.
    pub fn stop(&self) {
        self.stopped.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(r) = self.receiver.get() {
            r.stop();
        }
    }

    /// Whether a transfer is being offered or received.
    pub fn busy(&self) -> bool {
        self.receiver.get().is_some_and(|r| r.busy())
    }

    pub fn cancel_incoming(&self, session: &str) {
        if let Some(r) = self.receiver.get() {
            r.cancel(session);
        }
    }

    pub fn peers(&self) -> Vec<Peer> {
        let mut peers = self.peers.lock().unwrap_or_else(|e| e.into_inner());
        peers.retain(|_, p| p.seen.elapsed() < FORGET);
        let mut list: Vec<Peer> = peers.values().cloned().collect();
        list.sort_by_key(|p| p.info.alias.to_lowercase());
        list
    }

    fn heard(self: &Arc<Self>, info: DeviceInfo, addr: IpAddr, verified: bool, answer: bool) {
        let key = info.fingerprint.to_ascii_uppercase();
        if key == self.identity.fingerprint || key.is_empty() {
            return;
        }
        if verified {
            self.note_reach(&key, addr, info.port);
        }
        // The local network first: a device seen there recently keeps that address.
        let lan = self.peer(&key).filter(|p| !tailnet::is_tailscale(&p.addr) && p.seen.elapsed() < Duration::from_secs(120));
        let (addr, info) = match lan {
            Some(o) if tailnet::is_tailscale(&addr) => (o.addr, DeviceInfo { port: o.info.port, ..info }),
            _ => (addr, info),
        };
        let changed = {
            let mut peers = self.peers.lock().unwrap_or_else(|e| e.into_inner());
            let old = peers.get(&key).cloned();
            let verified = verified || old.as_ref().is_some_and(|p| p.verified && p.addr == addr);
            let changed = old.as_ref().is_none_or(|p| p.info.alias != info.alias || p.addr != addr || p.verified != verified);
            peers.insert(key.clone(), Peer { info: info.clone(), addr, verified, seen: Instant::now() });
            changed
        };
        if changed {
            self.events.peers_changed();
        }
        if answer {
            let me = self.clone();
            std::thread::spawn(move || me.answer(info, addr));
        }
    }

    /// Answer an announcement: register over HTTPS (which also proves who it
    /// is), or by multicast if that fails.
    fn answer(self: &Arc<Self>, info: DeviceInfo, addr: IpAddr) {
        let target = sender::Target { fingerprint: info.fingerprint.clone(), addr, port: info.port };
        if info.protocol == "https"
            && let Ok(theirs) = sender::register(&self.identity, &self.me(), &target)
        {
            self.heard(DeviceInfo { port: info.port, protocol: info.protocol, ..theirs }, addr, true, false);
            return;
        }
        if let Some(mc) = &self.multicast {
            mc.announce(&self.me(), false);
        }
    }

    pub(crate) fn peer(&self, fingerprint: &str) -> Option<Peer> {
        self.peers.lock().unwrap_or_else(|e| e.into_inner()).get(&fingerprint.to_ascii_uppercase()).cloned()
    }
}

/// Nearby LocalSend devices as Share destinations.
pub struct NearbyProvider(pub Arc<Nearby>);

pub const PREFIX: &str = "localsend:";

fn icon(device_type: Option<&str>) -> &'static str {
    match device_type {
        Some("mobile") => "phone-symbolic",
        Some("web") => "web-browser-symbolic",
        Some("headless" | "server") => "network-server-symbolic",
        _ => "computer-symbolic",
    }
}

impl Provider for NearbyProvider {
    fn id(&self) -> &'static str {
        "localsend"
    }

    fn name(&self) -> &'static str {
        "Nearby (LocalSend)"
    }

    fn kinds(&self) -> Vec<ContentKind> {
        vec![ContentKind::Text, ContentKind::Url, ContentKind::Image, ContentKind::File]
    }

    fn destinations(&self) -> Vec<Destination> {
        self.0
            .peers()
            .into_iter()
            .filter(|p| self.0.computer_with_key(&p.info.fingerprint).is_none())
            .map(|p| {
                let encrypted = p.info.protocol == "https";
                Destination {
                    id: format!("{PREFIX}{}", p.info.fingerprint.to_ascii_uppercase()),
                    provider: self.id().into(),
                    name: p.info.alias.clone(),
                    icon: icon(p.info.device_type.as_deref()).into(),
                    tint: "teal".into(),
                    section: Section::Nearby,
                    kinds: self.kinds(),
                    max_items: None,
                    needs_folder: false,
                    available: encrypted,
                    status: Some(match (encrypted, tailnet::is_tailscale(&p.addr)) {
                        (false, _) => "Encryption is off on that device".into(),
                        (true, true) => "Tailscale".into(),
                        (true, false) => "LocalSend".into(),
                    }),
                    pinned: false,
                    hidden: false,
                    default: false,
                }
            })
            .collect()
    }

    fn send(&self, destination: &str, task: &Task<'_>) -> Result<Outcome, SendError> {
        let fingerprint = destination.strip_prefix(PREFIX).unwrap_or(destination);
        let peer = self.0.peer(fingerprint).ok_or_else(|| SendError::failed("That device isn't nearby any more"))?;
        let target = sender::Target { fingerprint: fingerprint.to_string(), addr: peer.addr, port: peer.info.port };
        deliver(&self.0, &target, &peer.info.alias, task)
    }
}

/// Send a task's items to one LocalSend device.
pub(crate) fn deliver(nearby: &Nearby, target: &sender::Target, who: &str, task: &Task<'_>) -> Result<Outcome, SendError> {
    let files: Vec<sender::Outgoing<'_>> = task
        .items
        .iter()
        .map(|item| match &item.data {
            Data::Text(text) => sender::Outgoing {
                name: format!("{}.txt", uuid::Uuid::new_v4()),
                size: text.len() as u64,
                mime: "text/plain".into(),
                preview: Some(text.clone()),
                source: sender::Source::Bytes(text.as_bytes()),
            },
            Data::File(file) => sender::Outgoing {
                // LocalSend keeps a shared folder's layout through the name.
                name: nyx_share_providers::folder_of(item)
                    .into_iter()
                    .chain([nyx_share_core::names::sanitize(&item.info.name)])
                    .collect::<Vec<_>>()
                    .join("/"),
                size: item.info.size,
                mime: item.info.mime.clone().unwrap_or_else(|| "application/octet-stream".into()),
                preview: None,
                source: sender::Source::File(file),
            },
        })
        .collect();
    match sender::send(&nearby.identity, &nearby.me(), target, &files, task.cancel, task.progress) {
        Ok(0) if files.iter().all(|f| f.preview.is_some()) => {
            Ok(Outcome { message: format!("Sent to {who}"), outputs: Vec::new() })
        }
        Ok(0) => Err(SendError::failed(format!("{who} didn't take any of the files"))),
        Ok(n) if n < files.len() => {
            Ok(Outcome { message: format!("{who} took {n} of {} items", files.len()), outputs: Vec::new() })
        }
        Ok(n) => Ok(Outcome {
            message: format!("Sent {} to {who}", if n == 1 { "1 item".into() } else { format!("{n} items") }),
            outputs: Vec::new(),
        }),
        Err(sender::Error::Cancelled) => Err(SendError::Cancelled { outputs: Vec::new() }),
        Err(sender::Error::Declined) => Err(SendError::failed(format!("{who} declined"))),
        Err(e) => Err(SendError::failed(e.to_string())),
    }
}
