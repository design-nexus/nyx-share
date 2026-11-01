//! A blocking client for the Nyx Share service, for apps and tools in any toolkit.
//!
//! ```no_run
//! use nyx_share_client::{Client, Content, ShareOptions};
//! let client = Client::connect()?;
//! let mut content = Content::new();
//! content.file("/home/me/Pictures/photo.jpg")?;
//! client.share(&content, &ShareOptions { app_name: Some("Photos".into()), ..Default::default() })?;
//! # Ok::<(), nyx_share_client::Error>(())
//! ```
//!
//! Calls block, so make them from a worker thread, never a UI thread.

pub use nyx_share_core::{ContentKind, Destination, Direction, Job, JobState, PreferencesUpdate};
pub use nyx_share_dbus::{
    BUS, Capabilities, ComputerInfo, INTERFACE, NasInfo, NearbyStatus, NearbyUpdate, PATH, ProviderInfo, RequestInfo,
    ShareOptions,
};

use nyx_share_core::ItemSpec;
use nyx_share_dbus::{ShareProxyBlocking, ShareSheetProxyBlocking};
use std::fmt;
use std::fs::File;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;
use zbus::zvariant::Fd;

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)] // short-lived; boxing would change the public type
pub enum Event {
    JobChanged(Job),
    DestinationsChanged,
    NearbyChanged,
}

#[derive(Debug)]
pub enum Error {
    /// The service couldn't be reached.
    Unavailable(String),
    /// The service refused the request: bad content, or not allowed for this caller.
    Refused(String),
    /// No such job or request.
    NotFound(String),
    Failed(String),
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(m) => write!(f, "Nyx Share isn't available: {m}"),
            Self::Refused(m) | Self::NotFound(m) | Self::Failed(m) => f.write_str(m),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<zbus::Error> for Error {
    fn from(e: zbus::Error) -> Self {
        use zbus::fdo::Error as F;
        match e {
            zbus::Error::FDO(fdo) => match *fdo {
                F::InvalidArgs(m) | F::AccessDenied(m) => Self::Refused(m),
                F::UnknownObject(m) => Self::NotFound(m),
                F::ServiceUnknown(m)
                | F::NameHasNoOwner(m)
                | F::NoReply(m)
                | F::SpawnFailed(m)
                | F::SpawnExecFailed(m)
                | F::SpawnChildExited(m) => Self::Unavailable(m),
                F::Failed(m) => Self::Failed(m),
                other => Self::Failed(other.to_string()),
            },
            zbus::Error::MethodError(name, msg, _) => {
                let msg = msg.unwrap_or_else(|| name.to_string());
                match name.as_str() {
                    "org.freedesktop.DBus.Error.InvalidArgs" | "org.freedesktop.DBus.Error.AccessDenied" => Self::Refused(msg),
                    "org.freedesktop.DBus.Error.UnknownObject" => Self::NotFound(msg),
                    "org.freedesktop.DBus.Error.ServiceUnknown" | "org.freedesktop.DBus.Error.NameHasNoOwner" => {
                        Self::Unavailable(msg)
                    }
                    _ => Self::Failed(msg),
                }
            }
            zbus::Error::InputOutput(e) => Self::Unavailable(e.to_string()),
            other => Self::Failed(other.to_string()),
        }
    }
}

fn json<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, Error> {
    serde_json::from_str(s).map_err(|e| Error::Failed(format!("The service sent an unexpected reply: {e}")))
}

fn to_json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// What to share. Files are opened read-only here and sent as descriptors, so
/// the service never needs access to the caller's paths.
#[derive(Default)]
pub struct Content {
    items: Vec<ItemSpec>,
    files: Vec<File>,
}

impl Content {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn kinds(&self) -> Vec<ContentKind> {
        nyx_share_core::kinds_of(self.items.iter().map(|i| &i.kind))
    }

    pub fn text(&mut self, text: impl Into<String>) -> &mut Self {
        self.items.push(ItemSpec::text(text));
        self
    }

    pub fn url(&mut self, url: impl Into<String>) -> &mut Self {
        self.items.push(ItemSpec::url(url));
        self
    }

    /// Add a regular file. Images are recognised by their extension.
    pub fn file(&mut self, path: impl AsRef<Path>) -> Result<&mut Self, Error> {
        let path = path.as_ref();
        let file = File::open(path)?;
        if !file.metadata()?.is_file() {
            return Err(Error::Refused(format!("{} isn't a file. Folders can't be shared yet.", path.display())));
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        let mime = name.as_deref().and_then(nyx_share_core::mime::guess).map(str::to_string);
        let kind = if mime.as_deref().is_some_and(|m| m.starts_with("image/")) { ContentKind::Image } else { ContentKind::File };
        Ok(self.open_file(file, kind, name, mime))
    }

    /// Add an already open file, such as one received from a portal. It must be
    /// opened read-only.
    pub fn open_file(&mut self, file: File, kind: ContentKind, name: Option<String>, mime: Option<String>) -> &mut Self {
        let index = self.files.len() as u32;
        self.items.push(ItemSpec::fd(kind, index, name, mime));
        self.files.push(file);
        self
    }

    fn parts(&self) -> (String, Vec<Fd<'_>>) {
        (to_json(&self.items), self.files.iter().map(Fd::from).collect())
    }
}

pub struct Client {
    proxy: ShareProxyBlocking<'static>,
    conn: zbus::blocking::Connection,
}

impl Client {
    /// Connect to the session bus. The service starts on demand.
    pub fn connect() -> Result<Self, Error> {
        let conn = zbus::blocking::Connection::session().map_err(|e| Error::Unavailable(e.to_string()))?;
        let proxy = ShareProxyBlocking::new(&conn)?;
        Ok(Self { proxy, conn })
    }

    pub fn version(&self) -> Result<u32, Error> {
        Ok(self.proxy.version()?)
    }

    pub fn capabilities(&self) -> Result<Capabilities, Error> {
        json(&self.proxy.get_capabilities()?)
    }

    /// Open the Share Sheet. Returns the request id.
    pub fn share(&self, content: &Content, options: &ShareOptions) -> Result<String, Error> {
        let (items, fds) = content.parts();
        Ok(self.proxy.share(&items, fds, &to_json(options))?)
    }

    /// Send to one destination without the Share Sheet. Returns the job id.
    pub fn share_to(&self, destination: &str, content: &Content, options: &ShareOptions) -> Result<String, Error> {
        let (items, fds) = content.parts();
        Ok(self.proxy.share_to(destination, &items, fds, &to_json(options))?)
    }

    /// Destinations for `count` items of `kinds`; every destination if `kinds` is empty.
    pub fn destinations(&self, kinds: &[ContentKind], count: usize) -> Result<Vec<Destination>, Error> {
        let kinds: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
        json(&self.proxy.list_destinations(kinds, count as u32)?)
    }

    pub fn jobs(&self) -> Result<Vec<Job>, Error> {
        json(&self.proxy.list_jobs()?)
    }

    pub fn job(&self, id: &str) -> Result<Job, Error> {
        json(&self.proxy.get_job(id)?)
    }

    pub fn cancel(&self, id: &str) -> Result<(), Error> {
        Ok(self.proxy.cancel_job(id)?)
    }

    pub fn set_preferences(&self, update: &PreferencesUpdate) -> Result<(), Error> {
        Ok(self.proxy.set_destination_preferences(&to_json(update))?)
    }

    pub fn scan(&self) -> Result<(), Error> {
        Ok(self.proxy.scan()?)
    }

    /// Keep the service running while this connection lasts.
    pub fn hold(&self) -> Result<(), Error> {
        Ok(self.proxy.hold()?)
    }

    pub fn nearby(&self) -> Result<NearbyStatus, Error> {
        json(&self.proxy.get_nearby()?)
    }

    pub fn set_nearby(&self, update: &NearbyUpdate) -> Result<(), Error> {
        Ok(self.proxy.set_nearby(&to_json(update))?)
    }

    pub fn computers(&self) -> Result<Vec<ComputerInfo>, Error> {
        json(&self.proxy.list_computers()?)
    }

    /// Save a computer from a nearby destination id or an address.
    pub fn add_computer(&self, from: &str) -> Result<ComputerInfo, Error> {
        json(&self.proxy.add_computer(from)?)
    }

    pub fn save_computer(&self, computer: &ComputerInfo) -> Result<(), Error> {
        Ok(self.proxy.save_computer(&to_json(computer))?)
    }

    pub fn remove_computer(&self, id: &str) -> Result<(), Error> {
        Ok(self.proxy.remove_computer(id)?)
    }

    pub fn nas(&self) -> Result<Vec<NasInfo>, Error> {
        json(&self.proxy.list_nas()?)
    }

    /// Add (empty id) or change a NAS folder; returns its id.
    pub fn save_nas(&self, folder: &NasInfo) -> Result<String, Error> {
        let mut v = serde_json::to_value(folder).unwrap_or_default();
        if folder.conflict.is_empty() {
            v["conflict"] = "keep-both".into();
        }
        Ok(self.proxy.save_nas(&v.to_string())?)
    }

    pub fn remove_nas(&self, id: &str) -> Result<(), Error> {
        Ok(self.proxy.remove_nas(id)?)
    }

    pub fn test_nas(&self, id: &str) -> Result<String, Error> {
        Ok(self.proxy.test_nas(id)?)
    }

    /// Block until job `id` finishes, calling `progress` on each change.
    pub fn wait(&self, id: &str, mut progress: impl FnMut(&Job)) -> Result<Job, Error> {
        let (tx, rx) = mpsc::channel();
        let signals = self.proxy.receive_job_changed()?;
        let want = id.to_string();
        std::thread::spawn(move || {
            for signal in signals {
                let Ok(args) = signal.args() else { continue };
                if args.id == want && tx.send(args.job.to_string()).is_err() {
                    break;
                }
            }
        });
        let mut job = self.job(id)?;
        loop {
            progress(&job);
            if job.state.finished() {
                return Ok(job);
            }
            // Signals carry progress; the periodic check notices a service that went away.
            job = match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(s) => json(&s)?,
                Err(_) => self.job(id)?,
            };
        }
    }

    /// The service's changes as they happen. The iterator blocks between
    /// events; read it on a worker thread.
    pub fn events(&self) -> Result<impl Iterator<Item = Event> + Send + 'static, Error> {
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(BUS)?
            .path(PATH)?
            .interface(INTERFACE)?
            .build();
        let messages = zbus::blocking::MessageIterator::for_match_rule(rule, &self.conn, Some(64))?;
        Ok(messages.filter_map(|m| {
            let m = m.ok()?;
            let header = m.header();
            match header.member()?.as_str() {
                "JobChanged" => {
                    let (_, job): (String, String) = m.body().deserialize().ok()?;
                    serde_json::from_str(&job).ok().map(Event::JobChanged)
                }
                "DestinationsChanged" => Some(Event::DestinationsChanged),
                "NearbyChanged" => Some(Event::NearbyChanged),
                _ => None,
            }
        }))
    }

    /// The Share Sheet's private interface.
    pub fn sheet(&self) -> Result<ShareSheetProxyBlocking<'static>, Error> {
        Ok(ShareSheetProxyBlocking::new(&self.conn)?)
    }
}
