use crate::state::{Answer, Request, State, lock};
use crate::{caller, items, launch, nas, nearby};
use nyx_share_core::{API_VERSION, ContentKind, Limits, PreferencesUpdate, kinds_of};
use nyx_share_dbus::{
    Capabilities, IncomingFile, IncomingInfo, NearbyStatus, NearbyUpdate, ProviderInfo, RequestInfo, ShareOptions,
};
use nyx_share_providers::{Payload, SendOptions};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedFd;
use zbus::{Connection, fdo, interface};

fn json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

fn parse_options(s: &str) -> fdo::Result<ShareOptions> {
    if s.trim().is_empty() {
        return Ok(ShareOptions::default());
    }
    serde_json::from_str(s).map_err(|e| fdo::Error::InvalidArgs(format!("The options aren't valid JSON: {e}")))
}

/// A display name for the calling app: short and printable.
fn app_name(options: &ShareOptions) -> Option<String> {
    let name: String = options.app_name.as_deref()?.chars().filter(|c| !c.is_control()).take(64).collect();
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn send_options(options: &ShareOptions, sandboxed: bool) -> fdo::Result<SendOptions> {
    let folder = match options.folder.as_deref() {
        None => None,
        Some(_) if sandboxed => {
            return Err(fdo::Error::AccessDenied("Sandboxed apps can't choose a folder; use Share to let the user pick".into()));
        }
        Some(f) => {
            let p = PathBuf::from(f);
            if !p.is_absolute() {
                return Err(fdo::Error::InvalidArgs("The folder must be an absolute path".into()));
            }
            Some(p)
        }
    };
    let subfolder = options.subfolder.clone().filter(|s| !s.trim().is_empty());
    if let Some(sub) = &subfolder {
        nyx_share_nas::clean_subfolder(sub).map_err(fdo::Error::InvalidArgs)?;
    }
    Ok(SendOptions { folder, subfolder })
}

/// Run blocking work (network checks) off the D-Bus executor.
async fn blocking_task<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(work());
    });
    rx.recv().await.expect("the worker sends a result")
}

fn random_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// Compare without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub struct Share1(pub Arc<State>);

#[interface(name = "io.github.design_nexus.nyx.Share1")]
impl Share1 {
    #[zbus(property)]
    fn version(&self) -> u32 {
        API_VERSION
    }

    fn get_capabilities(&self) -> String {
        self.0.touch();
        let limits = Limits::default();
        let providers = self
            .0
            .providers()
            .iter()
            .map(|p| {
                let dests = p.destinations();
                let status = dests.iter().find_map(|d| d.status.clone().filter(|_| !d.available));
                ProviderInfo {
                    id: p.id().into(),
                    name: p.name().into(),
                    kinds: p.kinds(),
                    available: dests.iter().any(|d| d.available),
                    status,
                }
            })
            .collect();
        json(&Capabilities {
            version: API_VERSION,
            providers,
            max_items: limits.max_items as u32,
            max_text_bytes: limits.max_text_bytes as u32,
        })
    }

    async fn share(
        &self,
        items: &str,
        fds: Vec<OwnedFd>,
        options: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<String> {
        self.0.touch();
        let caller = caller::identify(conn, &header).await?;
        let options = parse_options(options)?;
        let payloads = items::build(items::parse_specs(items)?, fds, &caller)?;
        let id = uuid::Uuid::new_v4().to_string();
        let token = random_token();
        self.0.requests.lock().unwrap_or_else(|e| e.into_inner()).insert(
            id.clone(),
            Request {
                token: token.clone(),
                items: Arc::new(payloads),
                app_name: app_name(&options),
                sandboxed: caller.sandboxed,
                created: Instant::now(),
                jobs: Vec::new(),
            },
        );
        if let Some(n) = self.0.nearby() {
            nearby::look(&n);
        }
        if let Err(e) = launch::sheet(&id, &token) {
            self.0.requests.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            return Err(fdo::Error::Failed(e));
        }
        Ok(id)
    }

    async fn share_to(
        &self,
        destination: &str,
        items: &str,
        fds: Vec<OwnedFd>,
        options: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<String> {
        self.0.touch();
        let caller = caller::identify(conn, &header).await?;
        let options = parse_options(options)?;
        let send = send_options(&options, caller.sandboxed)?;
        let payloads = items::build(items::parse_specs(items)?, fds, &caller)?;
        let quiet_for = Duration::from_secs(options.notify_after.unwrap_or(0).into());
        self.0.start(destination, Arc::new(payloads), send, options.notify.unwrap_or(true), quiet_for)
    }

    fn list_destinations(&self, kinds: Vec<String>, count: u32) -> fdo::Result<String> {
        self.0.touch();
        if kinds.is_empty() {
            return Ok(json(&self.0.destinations()));
        }
        let kinds: Vec<ContentKind> = kinds
            .iter()
            .map(|k| ContentKind::parse(k).ok_or_else(|| fdo::Error::InvalidArgs(format!("Unknown content kind {k}"))))
            .collect::<fdo::Result<_>>()?;
        Ok(json(&self.0.destinations_for(&kinds_of(&kinds), count as usize)))
    }

    fn list_jobs(&self) -> String {
        self.0.touch();
        json(&self.0.jobs())
    }

    fn get_job(&self, id: &str) -> fdo::Result<String> {
        self.0.touch();
        self.0.job(id).map(|j| json(&j)).ok_or_else(|| fdo::Error::UnknownObject(format!("There is no job {id}")))
    }

    fn cancel_job(&self, id: &str) -> fdo::Result<()> {
        self.0.touch();
        if self.0.cancel(id) { Ok(()) } else { Err(fdo::Error::UnknownObject(format!("There is no job {id}"))) }
    }

    async fn set_destination_preferences(
        &self,
        update: &str,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        self.0.touch();
        let update: PreferencesUpdate = serde_json::from_str(update)
            .map_err(|e| fdo::Error::InvalidArgs(format!("The preferences aren't valid JSON: {e}")))?;
        self.0.update_prefs(update).map_err(|e| fdo::Error::Failed(format!("Couldn't save the destinations: {e}")))?;
        let _ = Self::destinations_changed(&emitter).await;
        Ok(())
    }

    fn scan(&self) {
        self.0.touch();
        if let Some(n) = self.0.nearby() {
            nearby::look(&n);
        }
    }

    async fn hold(&self, #[zbus(header)] header: Header<'_>) -> fdo::Result<()> {
        self.0.touch();
        let sender = header.sender().ok_or_else(|| fdo::Error::AccessDenied("The caller has no bus name".into()))?;
        lock(&self.0.holders).insert(sender.to_string());
        Ok(())
    }

    fn get_nearby(&self) -> String {
        self.0.touch();
        let settings = nearby::settings();
        let running = self.0.nearby();
        json(&NearbyStatus {
            enabled: running.is_some(),
            visibility: if settings.visibility == nearby::Visibility::Off { "off".into() } else { "running".into() },
            alias: running.as_ref().map(|n| n.me().alias).unwrap_or_else(|| nearby::alias(&settings)),
            fingerprint: running.as_ref().map(|n| n.fingerprint().to_string()).unwrap_or_else(nearby::fingerprint),
            receive_folder: nearby::receive_folder().display().to_string(),
            problem: nearby::problem().filter(|_| settings.visibility != nearby::Visibility::Off),
            tailscale: settings.tailscale,
            tailscale_state: nyx_share_localsend::tailnet::status().label().into(),
        })
    }

    fn set_nearby(&self, update: &str) -> fdo::Result<()> {
        self.0.touch();
        let update: NearbyUpdate =
            serde_json::from_str(update).map_err(|e| fdo::Error::InvalidArgs(format!("The settings aren't valid JSON: {e}")))?;
        nearby::update(&self.0, update).map_err(|e| match e.kind() {
            std::io::ErrorKind::InvalidInput => fdo::Error::InvalidArgs(e.to_string()),
            _ => fdo::Error::Failed(format!("Couldn't save the Nearby settings: {e}")),
        })
    }

    /// JSON array of saved computers, each with `presence`: `lan`, `tailscale`, `offline` or `unknown`.
    fn list_computers(&self) -> String {
        self.0.touch();
        let running = self.0.nearby();
        let list = running.as_ref().map(|n| n.computers()).unwrap_or_else(nearby::load_computers);
        let out: Vec<serde_json::Value> = list
            .into_iter()
            .map(|c| {
                let presence = running.as_ref().map(|n| n.presence(&c.id));
                let mut v = serde_json::to_value(&c).unwrap_or_default();
                v["presence"] = serde_json::to_value(presence.unwrap_or(nyx_share_localsend::computers::Presence::Unknown))
                    .unwrap_or_default();
                v
            })
            .collect();
        json(&out)
    }

    /// Save a computer from a nearby device (`localsend:<key>`) or an address
    /// (IP or name, optionally `:port`). Returns it as JSON.
    async fn add_computer(&self, from: &str) -> fdo::Result<String> {
        self.0.touch();
        let n = self.0.nearby().ok_or_else(|| fdo::Error::Failed("Turn on nearby sharing first".into()))?;
        let from = from.to_string();
        let c = {
            let _call = self.0.call();
            blocking_task(move || n.new_computer(&from)).await.map_err(fdo::Error::Failed)?
        };
        let mut list = nearby::load_computers();
        if let Some(existing) = list.iter().find(|x| x.fingerprint.eq_ignore_ascii_case(&c.fingerprint)) {
            return Err(fdo::Error::Failed(format!("{} is already saved as {}", c.name, existing.name)));
        }
        list.push(c.clone());
        nearby::save_computers(&self.0, list).map_err(|e| fdo::Error::Failed(format!("Couldn't save: {e}")))?;
        Ok(json(&c))
    }

    /// Change a saved computer (its key can't change).
    fn save_computer(&self, computer: &str) -> fdo::Result<()> {
        self.0.touch();
        let c: nyx_share_localsend::computers::Computer =
            serde_json::from_str(computer).map_err(|e| fdo::Error::InvalidArgs(format!("Not a computer: {e}")))?;
        let mut list = nearby::load_computers();
        let slot = list.iter_mut().find(|x| x.id == c.id).ok_or_else(|| fdo::Error::UnknownObject("No such computer".into()))?;
        if !slot.fingerprint.eq_ignore_ascii_case(&c.fingerprint) {
            return Err(fdo::Error::InvalidArgs("A computer's key can't change; remove it and add it again".into()));
        }
        if c.name.trim().is_empty() || c.receive_folder.as_ref().is_some_and(|f| !f.is_absolute()) {
            return Err(fdo::Error::InvalidArgs("A name is needed, and the folder must be an absolute path".into()));
        }
        *slot = c;
        nearby::save_computers(&self.0, list).map_err(|e| fdo::Error::Failed(format!("Couldn't save: {e}")))
    }

    fn remove_computer(&self, id: &str) -> fdo::Result<()> {
        self.0.touch();
        let mut list = nearby::load_computers();
        let before = list.len();
        list.retain(|c| c.id != id);
        if list.len() == before {
            return Err(fdo::Error::UnknownObject("No such computer".into()));
        }
        nearby::save_computers(&self.0, list).map_err(|e| fdo::Error::Failed(format!("Couldn't save: {e}")))
    }

    /// JSON array of NAS folders. Each has `has_password` (read only).
    fn list_nas(&self) -> String {
        self.0.touch();
        let list = self.0.nas.read().unwrap_or_else(|e| e.into_inner()).clone();
        let out: Vec<serde_json::Value> = list
            .into_iter()
            .map(|n| {
                let mut v = serde_json::to_value(&n).unwrap_or_default();
                v["has_password"] = nyx_share_nas::secrets::password(&n.id).is_some().into();
                v
            })
            .collect();
        json(&out)
    }

    /// Add (empty `id`) or change a NAS folder. Returns its id. Store its
    /// password in the Secret Service first (attributes application=nyx-share, nas=<id>).
    async fn save_nas(&self, folder: &str, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) -> fdo::Result<String> {
        self.0.touch();
        let mut n: nyx_share_nas::Nas =
            serde_json::from_str(folder).map_err(|e| fdo::Error::InvalidArgs(format!("Not a NAS folder: {e}")))?;
        n.validate().map_err(fdo::Error::InvalidArgs)?;
        if n.id.is_empty() {
            n.id = uuid::Uuid::new_v4().to_string();
        }
        let mut list = self.0.nas.read().unwrap_or_else(|e| e.into_inner()).clone();
        match list.iter_mut().find(|x| x.id == n.id) {
            Some(slot) => *slot = n.clone(),
            None => list.push(n.clone()),
        }
        nas::save(&self.0.nas, list).map_err(|e| fdo::Error::Failed(format!("Couldn't save: {e}")))?;
        let _ = Self::destinations_changed(&emitter).await;
        Ok(n.id)
    }

    async fn remove_nas(&self, id: &str, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) -> fdo::Result<()> {
        self.0.touch();
        let mut list = self.0.nas.read().unwrap_or_else(|e| e.into_inner()).clone();
        let before = list.len();
        list.retain(|n| n.id != id);
        if list.len() == before {
            return Err(fdo::Error::UnknownObject("No such NAS folder".into()));
        }
        nas::save(&self.0.nas, list).map_err(|e| fdo::Error::Failed(format!("Couldn't save: {e}")))?;
        nyx_share_nas::secrets::clear(id);
        let _ = Self::destinations_changed(&emitter).await;
        Ok(())
    }

    /// Connect and check the folder can be written to. Returns a sentence.
    async fn test_nas(&self, id: &str) -> fdo::Result<String> {
        self.0.touch();
        let n = self
            .0
            .nas
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|n| n.id == id)
            .cloned()
            .ok_or_else(|| fdo::Error::UnknownObject("No such NAS folder".into()))?;
        let _call = self.0.call();
        blocking_task(move || nyx_share_nas::test(&n)).await.map_err(fdo::Error::Failed)
    }

    #[zbus(signal)]
    async fn nearby_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn job_changed(emitter: &SignalEmitter<'_>, id: &str, job: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn destinations_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// The Share Sheet's side: every call needs the request's token.
pub struct Sheet1(pub Arc<State>);

impl Sheet1 {
    fn with<T>(&self, request: &str, token: &str, f: impl FnOnce(&mut Request) -> fdo::Result<T>) -> fdo::Result<T> {
        self.0.touch();
        let mut requests = self.0.requests.lock().unwrap_or_else(|e| e.into_inner());
        match requests.get_mut(request) {
            Some(r) if same(&r.token, token) => f(r),
            _ => Err(fdo::Error::AccessDenied("This share request has ended".into())),
        }
    }
}

#[interface(name = "io.github.design_nexus.nyx.ShareSheet1")]
impl Sheet1 {
    fn get_request(&self, request: &str, token: &str) -> fdo::Result<String> {
        let (items, app_name, sandboxed) =
            self.with(request, token, |r| Ok((r.items.clone(), r.app_name.clone(), r.sandboxed)))?;
        let kinds = kinds_of(items.iter().map(|p| &p.info.kind));
        Ok(json(&RequestInfo {
            app_name,
            items: items.iter().map(|p| p.info.clone()).collect(),
            destinations: self.0.destinations_for(&kinds, items.len()),
            sandboxed,
        }))
    }

    fn open_item(&self, request: &str, token: &str, index: u32) -> fdo::Result<OwnedFd> {
        let items = self.with(request, token, |r| Ok(r.items.clone()))?;
        let item: &Payload = items.get(index as usize).ok_or_else(|| fdo::Error::InvalidArgs("No such item".into()))?;
        match (&item.info.kind, &item.data) {
            (ContentKind::Image, nyx_share_providers::Data::File(file)) => {
                let dup = file.try_clone().map_err(|e| fdo::Error::Failed(e.to_string()))?;
                Ok(std::os::fd::OwnedFd::from(dup).into())
            }
            _ => Err(fdo::Error::InvalidArgs("Only images have a preview".into())),
        }
    }

    fn choose(&self, request: &str, token: &str, destination: &str, options: &str) -> fdo::Result<String> {
        let options = parse_options(options)?;
        // The folder comes from the user's own choice in the sheet, so a sandboxed origin doesn't matter here.
        let send = send_options(&options, false)?;
        let items = self.with(request, token, |r| Ok(r.items.clone()))?;
        let id = self.0.start(destination, items, send, false, Duration::ZERO)?;
        self.with(request, token, |r| {
            r.jobs.push(id.clone());
            Ok(())
        })?;
        Ok(id)
    }

    fn dismiss(&self, request: &str, token: &str) -> fdo::Result<()> {
        let jobs = self.with(request, token, |r| Ok(r.jobs.clone()))?;
        self.0.requests.lock().unwrap_or_else(|e| e.into_inner()).remove(request);
        self.0.release(&jobs);
        Ok(())
    }

    fn get_incoming(&self, session: &str, token: &str) -> fdo::Result<String> {
        self.0.touch();
        let incoming = lock(&self.0.incoming);
        let i = incoming.get(session).filter(|i| same(&i.token, token)).ok_or_else(ended)?;
        let o = &i.offer;
        let info = IncomingInfo {
            session: o.session.clone(),
            sender: o.sender.alias.clone(),
            device_type: o.sender.device_type.clone(),
            device_model: o.sender.device_model.clone(),
            fingerprint: o.sender.fingerprint.to_ascii_uppercase(),
            verified: o.verified,
            address: o.addr.to_string(),
            files: o
                .files
                .iter()
                .map(|f| IncomingFile {
                    name: f.file_name.clone(),
                    size: f.size,
                    mime: f.file_type.clone(),
                    text: f.preview.clone().filter(|_| f.file_type.starts_with("text/")),
                })
                .collect(),
            total: o.total,
            message_only: o.message_only,
            answer: serde_json::to_value(i.answer).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default(),
            folder: i.folder.clone(),
        };
        Ok(json(&info))
    }

    fn answer_incoming(&self, session: &str, token: &str, accept: bool) -> fdo::Result<()> {
        self.0.touch();
        let mut incoming = lock(&self.0.incoming);
        let i = incoming.get_mut(session).filter(|i| same(&i.token, token)).ok_or_else(ended)?;
        if i.answer != Answer::Pending {
            return Err(fdo::Error::Failed("This request was already answered or has expired".into()));
        }
        i.answer = if accept { Answer::Accepted } else { Answer::Declined };
        self.0.answered.notify_all();
        Ok(())
    }

    fn dismiss_incoming(&self, session: &str, token: &str) -> fdo::Result<()> {
        let mut incoming = lock(&self.0.incoming);
        let i = incoming.get_mut(session).filter(|i| same(&i.token, token)).ok_or_else(ended)?;
        i.dismissed = true;
        if i.answer == Answer::Pending {
            i.answer = Answer::Declined;
            self.0.answered.notify_all();
        }
        drop(incoming);
        self.0.release(&[session.to_string()]);
        Ok(())
    }
}

fn ended() -> fdo::Error {
    fdo::Error::AccessDenied("This share request has ended".into())
}
