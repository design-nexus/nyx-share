//! NAS folders: `$XDG_CONFIG_HOME/nyx-share/nas.toml`. Passwords are in the
//! Secret Service (see nyx_share_nas::secrets), never in this file.

use nyx_share_nas::Nas;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

#[derive(Default, Serialize, Deserialize)]
struct NasFile {
    #[serde(default, rename = "nas")]
    folders: Vec<Nas>,
}

fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("nyx-share/nas.toml")
}

pub fn load() -> Vec<Nas> {
    std::fs::read_to_string(path()).ok().and_then(|s| toml::from_str::<NasFile>(&s).ok()).map(|f| f.folders).unwrap_or_default()
}

pub fn save(store: &Arc<RwLock<Vec<Nas>>>, folders: Vec<Nas>) -> std::io::Result<()> {
    let path = path();
    std::fs::create_dir_all(path.parent().expect("has a parent"))?;
    let text = toml::to_string(&NasFile { folders: folders.clone() }).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, &path)?;
    *store.write().unwrap_or_else(|e| e.into_inner()) = folders;
    Ok(())
}
