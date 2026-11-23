//! Shell settings, kept in `files/shell.json` next to magpie's own config.
//!
//! The Java side reads exactly one field out of this file (`autostart`, in
//! `BootReceiver`) so a reboot can start the service without loading the native
//! library; everything else is read and written here.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellSettings {
    /// The port agents point at. 3425 is magpie's own default.
    #[serde(default = "default_gateway_port")]
    pub gateway_port: u16,
    /// The page port. Upstream's `magpie web` also uses 3430.
    #[serde(default = "default_web_port")]
    pub web_port: u16,
    /// Bind the gateway to 0.0.0.0 instead of loopback.
    #[serde(default)]
    pub lan: bool,
    /// Start on boot.
    #[serde(default)]
    pub autostart: bool,
    /// Look for a newer release on start-up.
    #[serde(default = "yes")]
    pub check_updates: bool,
    /// A version the user asked not to be told about again.
    #[serde(default)]
    pub skipped_version: String,
}

fn yes() -> bool {
    true
}
fn default_gateway_port() -> u16 {
    3425
}
fn default_web_port() -> u16 {
    3430
}

impl Default for ShellSettings {
    fn default() -> Self {
        Self {
            gateway_port: 3425,
            web_port: 3430,
            lan: false,
            autostart: false,
            check_updates: true,
            skipped_version: String::new(),
        }
    }
}

pub fn path(files_dir: &Path) -> PathBuf {
    files_dir.join("shell.json")
}

pub fn load(files_dir: &Path) -> ShellSettings {
    let p = path(files_dir);
    match std::fs::read_to_string(&p) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            magpie_core::warn!("shell.json is unreadable ({e}); using defaults");
            ShellSettings::default()
        }),
        Err(_) => ShellSettings::default(),
    }
}

pub fn save(files_dir: &Path, s: &ShellSettings) -> std::io::Result<()> {
    std::fs::create_dir_all(files_dir)?;
    std::fs::write(path(files_dir), serde_json::to_string_pretty(s)?)
}

/// The directory the app hands us on first start, cached so later callbacks
/// (a notification action, an update download) do not need Java again.
static FILES_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub fn set_files_dir(p: PathBuf) {
    let _ = FILES_DIR.set(p);
}

pub fn files_dir() -> Option<&'static PathBuf> {
    FILES_DIR.get()
}

pub fn settings() -> ShellSettings {
    files_dir().map(|d| load(d)).unwrap_or_default()
}
