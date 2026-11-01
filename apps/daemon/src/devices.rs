//! Linked devices: `$XDG_CONFIG_HOME/nyx-share/devices.toml`, one entry for a
//! device reached several ways. Only the service writes it.

use nyx_share_core::LinkedDevice;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Default, Serialize, Deserialize)]
struct DevicesFile {
    #[serde(default, rename = "device")]
    devices: Vec<LinkedDevice>,
}

fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("nyx-share/devices.toml")
}

pub fn load() -> Vec<LinkedDevice> {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|s| toml::from_str::<DevicesFile>(&s).ok())
        .map(|f| f.devices)
        .unwrap_or_default()
}

pub fn save(devices: &[LinkedDevice]) -> std::io::Result<()> {
    let path = path();
    std::fs::create_dir_all(path.parent().expect("has a parent"))?;
    let text = toml::to_string(&DevicesFile { devices: devices.to_vec() }).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, &path)
}
