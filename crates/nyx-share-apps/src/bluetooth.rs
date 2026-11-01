//! Bluetooth: paired devices that take files by OBEX Object Push, through
//! BlueZ's obexd (`org.bluez.obex`, session bus). Devices come from BlueZ on
//! the system bus; Share never pairs. The receiving device asks its user.

use nyx_share_core::{ContentKind, Destination, Section};
use nyx_share_providers::stage::Stage;
use nyx_share_providers::{Data, Outcome, Provider, SendError, Task};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

pub const PREFIX: &str = "bluetooth:";
/// The Object Push service, as devices advertise it.
const OPP: &str = "00001105-0000-1000-8000-00805f9b34fb";
const OBEX: &str = "org.bluez.obex";

#[derive(Clone, Debug)]
pub struct Device {
    pub address: String,
    pub name: String,
    pub connected: bool,
    pub icon: String,
}

type Objects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

/// Paired devices with Object Push, from BlueZ.
pub fn devices() -> Vec<Device> {
    let Ok(bus) = zbus::blocking::Connection::system() else { return Vec::new() };
    let Ok(reply) = bus.call_method(Some("org.bluez"), "/", Some("org.freedesktop.DBus.ObjectManager"), "GetManagedObjects", &())
    else {
        return Vec::new();
    };
    let Ok(objects) = reply.body().deserialize::<Objects>() else { return Vec::new() };
    let text = |p: &HashMap<String, OwnedValue>, k: &str| p.get(k).and_then(|v| String::try_from(v.clone()).ok());
    let flag = |p: &HashMap<String, OwnedValue>, k: &str| p.get(k).and_then(|v| bool::try_from(v.clone()).ok()).unwrap_or(false);
    let mut out: Vec<Device> = objects
        .values()
        .filter_map(|ifaces| ifaces.get("org.bluez.Device1"))
        .filter(|p| flag(p, "Paired"))
        .filter(|p| {
            p.get("UUIDs")
                .and_then(|v| Vec::<String>::try_from(v.clone()).ok())
                .is_some_and(|u| u.iter().any(|u| u.eq_ignore_ascii_case(OPP)))
        })
        .filter_map(|p| {
            Some(Device {
                address: text(p, "Address")?,
                name: text(p, "Alias").or_else(|| text(p, "Name")).unwrap_or_else(|| "Bluetooth device".into()),
                connected: flag(p, "Connected"),
                icon: text(p, "Icon").unwrap_or_default(),
            })
        })
        .collect();
    out.sort_by_key(|d| d.name.to_lowercase());
    out
}

/// Whether obexd can be reached (installed and activatable on the session bus).
fn obex_available(bus: &zbus::blocking::Connection) -> bool {
    let Ok(dbus) = zbus::blocking::fdo::DBusProxy::new(bus) else { return false };
    let Ok(name) = zbus::names::BusName::try_from(OBEX) else { return false };
    dbus.name_has_owner(name).unwrap_or(false)
        || dbus.list_activatable_names().is_ok_and(|names| names.iter().any(|n| n.as_str() == OBEX))
}

pub struct Bluetooth {
    stage: PathBuf,
}

impl Default for Bluetooth {
    fn default() -> Self {
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache"));
        Self { stage: cache.join("nyx-share/bluetooth") }
    }
}

fn icon(kind: &str) -> &'static str {
    match kind {
        "phone" => "phone-symbolic",
        "computer" => "computer-symbolic",
        _ => "bluetooth-symbolic",
    }
}

/// obexd's transfer changes: (object path, changed properties).
fn watch_transfers(
    bus: &zbus::blocking::Connection,
) -> zbus::Result<std::sync::mpsc::Receiver<(String, HashMap<String, OwnedValue>)>> {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(OBEX)?
        .interface("org.freedesktop.DBus.Properties")?
        .member("PropertiesChanged")?
        .add_arg("org.bluez.obex.Transfer1")?
        .build();
    let messages = zbus::blocking::MessageIterator::for_match_rule(rule, bus, Some(256))?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for message in messages.flatten() {
            let path = message.header().path().map(|p| p.to_string()).unwrap_or_default();
            let Ok((_, changed, _)) = message.body().deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>() else {
                continue;
            };
            if tx.send((path, changed)).is_err() {
                break;
            }
        }
    });
    Ok(rx)
}

/// A property of one obexd transfer.
fn property(bus: &zbus::blocking::Connection, transfer: &OwnedObjectPath, name: &str) -> Option<OwnedValue> {
    let reply = bus
        .call_method(
            Some(OBEX),
            transfer.as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.bluez.obex.Transfer1", name),
        )
        .ok()?;
    reply.body().deserialize::<OwnedValue>().ok()
}

impl Provider for Bluetooth {
    fn id(&self) -> &'static str {
        "bluetooth"
    }

    fn name(&self) -> &'static str {
        "Bluetooth"
    }

    fn kinds(&self) -> Vec<ContentKind> {
        vec![ContentKind::Text, ContentKind::Image, ContentKind::File]
    }

    fn destinations(&self) -> Vec<Destination> {
        let list = devices();
        if list.is_empty() {
            return Vec::new();
        }
        let obex = zbus::blocking::Connection::session().is_ok_and(|bus| obex_available(&bus));
        list.into_iter()
            .map(|d| Destination {
                id: format!("{PREFIX}{}", d.address),
                provider: self.id().into(),
                name: d.name,
                icon: icon(&d.icon).into(),
                tint: "blue".into(),
                section: Section::Devices,
                kinds: self.kinds(),
                max_items: None,
                needs_folder: false,
                needs_recipient: false,
                available: obex,
                status: Some(match (obex, d.connected) {
                    (false, _) => "Install bluez-obex to send files".into(),
                    (true, true) => "Bluetooth".into(),
                    (true, false) => "Bluetooth · not connected".into(),
                }),
                pinned: false,
                hidden: false,
                default: false,
            })
            .collect()
    }

    fn send(&self, destination: &str, task: &Task<'_>) -> Result<Outcome, SendError> {
        let address = destination.strip_prefix(PREFIX).unwrap_or(destination).to_string();
        let failed = SendError::failed;
        let device = devices()
            .into_iter()
            .find(|d| d.address == address)
            .ok_or_else(|| failed("That device isn't paired any more".into()))?;
        let bus = zbus::blocking::Connection::session().map_err(|e| failed(format!("No session bus: {e}")))?;
        if !obex_available(&bus) {
            return Err(failed("Sending files over Bluetooth needs bluez-obex (obexd)".into()));
        }
        let mut stage = Stage::new(&self.stage).map_err(|e| failed(format!("Couldn't prepare the files: {e}")))?;
        let mut files = Vec::new();
        for item in task.items {
            let path = match &item.data {
                Data::File(file) => stage.place(file, &item.info.name, item.info.size, task)?,
                Data::Text(text) => stage.write_text(&nyx_share_core::names::for_text(Some(&item.info.name)), text)?,
            };
            files.push(path);
        }
        (task.step)(Some(&format!("Connecting to {}…", device.name)));
        let args: HashMap<&str, Value> = HashMap::from([("Target", Value::from("opp"))]);
        let session: OwnedObjectPath = bus
            .call_method(
                Some(OBEX),
                "/org/bluez/obex",
                Some("org.bluez.obex.Client1"),
                "CreateSession",
                &(address.as_str(), args),
            )
            .and_then(|r| r.body().deserialize::<OwnedObjectPath>())
            .map_err(|e| failed(format!("Couldn't connect to {}: {e}", device.name)))?;
        // obexd drops a transfer as soon as it ends, so its last status comes
        // from the signal, not from asking.
        let changes = watch_transfers(&bus).map_err(|e| failed(format!("Couldn't follow the transfer: {e}")))?;
        let result = (|| {
            for (i, path) in files.iter().enumerate() {
                (task.step)(Some(&format!("Waiting for {} to accept…", device.name)));
                let (transfer, initial): (OwnedObjectPath, HashMap<String, OwnedValue>) = bus
                    .call_method(
                        Some(OBEX),
                        session.as_str(),
                        Some("org.bluez.obex.ObjectPush1"),
                        "SendFile",
                        &(path.display().to_string(),),
                    )
                    .and_then(|r| r.body().deserialize())
                    .map_err(|e| failed(format!("{} refused the file: {e}", device.name)))?;
                let mut status = initial.get("Status").and_then(|v| String::try_from(v.clone()).ok()).unwrap_or_default();
                let mut reported = 0u64;
                let start = Instant::now();
                loop {
                    if task.cancelled() {
                        let _ = bus.call_method(Some(OBEX), transfer.as_str(), Some("org.bluez.obex.Transfer1"), "Cancel", &());
                        return Err(SendError::Cancelled { outputs: Vec::new() });
                    }
                    let done = match changes.recv_timeout(Duration::from_millis(300)) {
                        Ok((from, changed)) if from == transfer.as_str() => {
                            if let Some(s) = changed.get("Status").and_then(|v| String::try_from(v.clone()).ok()) {
                                status = s;
                            }
                            changed.get("Transferred").and_then(|v| u64::try_from(v.clone()).ok())
                        }
                        Ok(_) => continue,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            property(&bus, &transfer, "Transferred").and_then(|v| u64::try_from(v).ok())
                        }
                        Err(_) => return Err(failed("Lost track of the transfer".into())),
                    };
                    if let Some(done) = done.filter(|d| *d > reported) {
                        (task.step)(None);
                        (task.progress)(done - reported);
                        reported = done;
                    }
                    match status.as_str() {
                        "complete" => break,
                        "error" => return Err(failed(format!("{} didn't take {}", device.name, task.items[i].info.name))),
                        _ if reported == 0 && start.elapsed() > Duration::from_secs(120) => {
                            let _ =
                                bus.call_method(Some(OBEX), transfer.as_str(), Some("org.bluez.obex.Transfer1"), "Cancel", &());
                            return Err(failed(format!("{} didn't accept within two minutes", device.name)));
                        }
                        _ => {}
                    }
                }
                let size = task.items[i].size();
                if size > reported {
                    (task.progress)(size - reported);
                }
            }
            Ok(())
        })();
        let _ = bus.call_method(Some(OBEX), "/org/bluez/obex", Some("org.bluez.obex.Client1"), "RemoveSession", &(&session,));
        drop(stage);
        result?;
        let what = match task.items {
            [one] => one.info.name.clone(),
            many => format!("{} items", many.len()),
        };
        Ok(Outcome { message: format!("Sent {what} to {} over Bluetooth", device.name), outputs: Vec::new() })
    }
}
