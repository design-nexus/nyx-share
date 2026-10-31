pub use nyx_theme::ThemeMode;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub mode: ThemeMode,
    pub theme: String,
    pub reduce_motion: bool,
    pub glow: bool,
    /// Where Save to folder last saved.
    pub last_folder: Option<String>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self { mode: ThemeMode::Omarchy, theme: "nyx-dark".into(), reduce_motion: false, glow: false, last_folder: None }
    }
}

nyx_core::prefs_store!(Prefs);
