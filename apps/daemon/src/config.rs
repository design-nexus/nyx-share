//! The user's destination arrangement: `$XDG_CONFIG_HOME/nyx-share/destinations.toml`.
//! Only the service writes it; Settings changes it over D-Bus.

use nyx_share_core::Preferences;
use std::path::PathBuf;

fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("nyx-share").join("destinations.toml")
}

pub fn load() -> Preferences {
    let path = path();
    match std::fs::read_to_string(&path) {
        Ok(s) => toml::from_str(&s).unwrap_or_else(|e| {
            eprintln!("nyx-share: ignoring {}: {e}", path.display());
            Preferences::default()
        }),
        Err(_) => Preferences::default(),
    }
}

pub fn save(prefs: &Preferences) -> std::io::Result<()> {
    let path = path();
    let dir = path.parent().expect("has a parent");
    std::fs::create_dir_all(dir)?;
    let text = toml::to_string(prefs).map_err(std::io::Error::other)?;
    let tmp = dir.join(format!(".destinations.toml.{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}
