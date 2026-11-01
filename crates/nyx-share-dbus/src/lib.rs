//! The Nyx Share D-Bus interfaces: names, proxies and the JSON carried in them.
//! See API.md. Structured values travel as JSON strings; consumers must ignore
//! fields they don't know. A breaking change gets a new interface version.

use nyx_share_core::{ContentKind, Destination, ItemInfo};
use serde::{Deserialize, Serialize};
use zbus::proxy;
use zbus::zvariant::{Fd, OwnedFd};

pub const BUS: &str = "io.github.design_nexus.nyx.Share1";
pub const PATH: &str = "/io/github/design_nexus/nyx/Share1";
pub const INTERFACE: &str = "io.github.design_nexus.nyx.Share1";
/// Private to the Share Sheet; not a stable API.
pub const SHEET_INTERFACE: &str = "io.github.design_nexus.nyx.ShareSheet1";

/// Environment variables that hand a request to the Share Sheet.
pub const ENV_REQUEST: &str = "NYX_SHARE_REQUEST";
pub const ENV_TOKEN: &str = "NYX_SHARE_TOKEN";
/// Instead of a request: an incoming offer to ask about.
pub const ENV_INCOMING: &str = "NYX_SHARE_INCOMING";

#[proxy(
    interface = "io.github.design_nexus.nyx.Share1",
    default_service = "io.github.design_nexus.nyx.Share1",
    default_path = "/io/github/design_nexus/nyx/Share1"
)]
pub trait Share {
    #[zbus(property)]
    fn version(&self) -> zbus::Result<u32>;

    /// JSON [`Capabilities`].
    fn get_capabilities(&self) -> zbus::Result<String>;

    /// Open the Share Sheet for `items` (JSON array of `ItemSpec`). Returns the request id.
    fn share(&self, items: &str, fds: Vec<Fd<'_>>, options: &str) -> zbus::Result<String>;

    /// Send straight to a destination, without the Share Sheet. Returns the job id.
    fn share_to(&self, destination: &str, items: &str, fds: Vec<Fd<'_>>, options: &str) -> zbus::Result<String>;

    /// JSON array of [`Destination`]s accepting `count` items of `kinds`, hidden
    /// ones included and marked. Empty `kinds` lists every destination.
    fn list_destinations(&self, kinds: Vec<&str>, count: u32) -> zbus::Result<String>;

    /// JSON array of this session's [`Job`]s, newest first.
    fn list_jobs(&self) -> zbus::Result<String>;

    fn get_job(&self, id: &str) -> zbus::Result<String>;

    fn cancel_job(&self, id: &str) -> zbus::Result<()>;

    fn retry_job(&self, id: &str) -> zbus::Result<String>;

    fn clear_history(&self) -> zbus::Result<()>;

    /// JSON [`nyx_share_core::PreferencesUpdate`]: the lists it contains replace the saved ones.
    fn set_destination_preferences(&self, update: &str) -> zbus::Result<()>;

    /// Ask nearby devices to answer now.
    fn scan(&self) -> zbus::Result<()>;

    /// Keep the service, and Nearby visibility, running while the caller is
    /// connected to the bus (the Share window does this while it's open).
    fn hold(&self) -> zbus::Result<()>;

    /// JSON [`NearbyStatus`].
    fn get_nearby(&self) -> zbus::Result<String>;

    /// JSON [`NearbyUpdate`]; the fields present are saved and applied.
    fn set_nearby(&self, update: &str) -> zbus::Result<()>;

    /// JSON array of [`ComputerInfo`].
    fn list_computers(&self) -> zbus::Result<String>;

    /// Save a computer from a nearby device (`localsend:<key>`) or an address.
    fn add_computer(&self, from: &str) -> zbus::Result<String>;

    /// JSON [`ComputerInfo`] (without `presence`); its key can't change.
    fn save_computer(&self, computer: &str) -> zbus::Result<()>;

    fn remove_computer(&self, id: &str) -> zbus::Result<()>;

    /// JSON array of [`NasInfo`].
    fn list_nas(&self) -> zbus::Result<String>;

    /// Add (empty id) or change a NAS folder; returns its id.
    fn save_nas(&self, folder: &str) -> zbus::Result<String>;

    fn remove_nas(&self, id: &str) -> zbus::Result<()>;

    /// Connect and check it can be written to; returns a sentence.
    fn test_nas(&self, id: &str) -> zbus::Result<String>;

    #[zbus(signal)]
    fn nearby_changed(&self) -> zbus::Result<()>;

    #[zbus(signal)]
    fn job_changed(&self, id: &str, job: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    fn destinations_changed(&self) -> zbus::Result<()>;
}

#[proxy(
    interface = "io.github.design_nexus.nyx.ShareSheet1",
    default_service = "io.github.design_nexus.nyx.Share1",
    default_path = "/io/github/design_nexus/nyx/Share1"
)]
pub trait ShareSheet {
    /// JSON [`RequestInfo`].
    fn get_request(&self, request: &str, token: &str) -> zbus::Result<String>;

    /// A read-only descriptor for an image item, for its preview.
    fn open_item(&self, request: &str, token: &str, index: u32) -> zbus::Result<OwnedFd>;

    /// Send the request's items to `destination`. Returns the job id.
    fn choose(&self, request: &str, token: &str, destination: &str, options: &str) -> zbus::Result<String>;

    /// The sheet closed. Running jobs continue and notify when they finish.
    fn dismiss(&self, request: &str, token: &str) -> zbus::Result<()>;

    /// JSON [`IncomingInfo`] for an offer from a nearby device.
    fn get_incoming(&self, session: &str, token: &str) -> zbus::Result<String>;

    /// Accept or decline the offer. Accepting starts the receive job (id = session).
    fn answer_incoming(&self, session: &str, token: &str, accept: bool) -> zbus::Result<()>;

    /// The sheet closed: an unanswered offer is declined; a transfer continues.
    fn dismiss_incoming(&self, session: &str, token: &str) -> zbus::Result<()>;
}

/// A NAS folder destination.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NasInfo {
    pub id: String,
    pub name: String,
    /// `smb://`, `sftp://`, `nfs://` or `file://` address of the folder.
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tailscale_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// `keep-both` or `skip`.
    pub conflict: String,
    /// A password is saved in the Secret Service. Read only.
    #[serde(skip_serializing)]
    pub has_password: bool,
    /// Write-only, for SaveNas: stored in the Secret Service (empty clears it),
    /// never in nas.toml and never listed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

/// A saved computer: its key, addresses and trust.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ComputerInfo {
    pub id: String,
    pub name: String,
    pub fingerprint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lan: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tailscale: Option<String>,
    pub port: u16,
    /// `auto`, `lan` or `tailscale`.
    pub prefer: String,
    /// Files from it are received without asking.
    pub trusted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receive_folder: Option<String>,
    /// `lan`, `tailscale`, `offline` or `unknown`. Read only.
    #[serde(skip_serializing)]
    pub presence: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NearbyStatus {
    /// Discoverable and receiving now.
    pub enabled: bool,
    /// The saved setting: `running` or `off`.
    pub visibility: String,
    pub alias: String,
    /// This device's key, uppercase hex; empty until Nearby has started once.
    pub fingerprint: String,
    pub receive_folder: String,
    /// Why Nearby isn't running, when it should be.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    /// Looking for LocalSend on tailnet devices is turned on.
    pub tailscale: bool,
    /// Tailscale on this computer: `running`, `stopped` or `missing`.
    pub tailscale_state: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NearbyUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receive_folder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tailscale: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IncomingFile {
    pub name: String,
    pub size: u64,
    pub mime: String,
    /// The text of a text message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IncomingInfo {
    pub session: String,
    pub sender: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_model: Option<String>,
    pub fingerprint: String,
    /// The sender proved it holds the certificate with this fingerprint.
    pub verified: bool,
    pub address: String,
    pub files: Vec<IncomingFile>,
    pub total: u64,
    /// Only text messages: shown, nothing saved.
    pub message_only: bool,
    /// `pending`, `accepted`, `declined` or `expired`.
    pub answer: String,
    pub folder: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Capabilities {
    pub version: u32,
    pub providers: Vec<ProviderInfo>,
    pub max_items: u32,
    pub max_text_bytes: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub kinds: Vec<ContentKind>,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Turned on in Settings → Sharing (`disabled` in the preferences).
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

/// Options for `Share`, `ShareTo` and `Choose`. Unknown keys are ignored.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShareOptions {
    /// Shown in the Share Sheet and in notifications ("from Files").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    /// The folder for Save to folder. Refused from sandboxed callers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// A folder under a NAS destination's own, for this send (`2026/Trips`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subfolder: Option<String>,
    /// Notify when the job finishes (default true).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify: Option<bool>,
    /// With `notify`: only if the job takes at least this many seconds, for a
    /// caller that reports quick results itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify_after: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    pub items: Vec<ItemInfo>,
    /// Compatible destinations, arranged, hidden ones marked.
    pub destinations: Vec<Destination>,
    pub sandboxed: bool,
}
