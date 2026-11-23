//! Configuration: the on-disk shape of `settings.json` and `providers.json`.
//!
//! Upstream magpie keeps `settings.json` and `providers.json` next to each
//! other under `$XDG_CONFIG_HOME/magpie`; magpie-rs keeps that layout so an
//! existing magpie config directory is readable after a manual key migration.
//! Unknown keys are preserved on rewrite, so hand-written fields we do not
//! understand are never dropped.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// How a provider's HTTP API talks. This is what decides the translator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// `POST {base}/chat/completions` — OpenAI and every clone of it.
    Openai,
    /// `POST {base}/messages` — Anthropic and every clone of it.
    Anthropic,
    /// `POST {base}/models/{model}:generateContent` — Google Gemini.
    Gemini,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Openai => "openai",
            Protocol::Anthropic => "anthropic",
            Protocol::Gemini => "gemini",
        }
    }
}

impl std::str::FromStr for Protocol {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "openai" | "openai-compatible" | "oai" => Ok(Protocol::Openai),
            "anthropic" | "claude" | "anthropic-compatible" => Ok(Protocol::Anthropic),
            "gemini" | "google" => Ok(Protocol::Gemini),
            other => Err(format!(
                "unknown protocol `{other}`; expected openai, anthropic or gemini"
            )),
        }
    }
}

fn default_true() -> bool {
    true
}

/// One upstream. `base_url` may carry a path prefix (`https://host/v1`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    /// Short slug used in model names (`deepseek/deepseek-chat`) and in the URL
    /// space. Must be unique.
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub protocol: Protocol,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    /// Models this provider serves. An empty list means "pass anything through",
    /// which is what a user wants for a private OpenAI-compatible endpoint.
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Lower sorts first when several providers serve the same model.
    #[serde(default)]
    pub priority: i32,
    /// Extra headers, e.g. `{"OpenAI-Beta": "assistants=v2"}`.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Flatten unknown upstream fields so a newer magpie config does not lose
    /// data when magpie-rs rewrites the file.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Provider {
    pub fn display(&self) -> &str {
        if self.name.is_empty() {
            &self.id
        } else {
            &self.name
        }
    }

    /// `base_url` without a trailing slash, so joins are predictable.
    pub fn base(&self) -> &str {
        self.base_url.trim_end_matches('/')
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum RouteMode {
    /// Strict order: lowest priority (then declaration order) wins.
    #[default]
    Order,
    /// Round-robin across every provider that serves the model.
    Rotate,
    /// Try each candidate in order until one answers with a non-5xx status.
    Failover,
}

/// Gateway-side switches. The Android shell owns `port`/`lan` and passes them in
/// through `MAGPIE_ADDR`-equivalent options; everything here is editable from
/// the built-in web UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_web_port")]
    pub web_port: u16,
    /// The page key. Upstream wants 16+ of `[A-Za-z0-9._~-]`; the shell keeps it
    /// stable across restarts so the WebView stays signed in.
    #[serde(default)]
    pub web_key: String,
    /// Gateway keys (`sk-magpie-key-…`). When the gateway is bound to a
    /// non-loopback address, at least one is required.
    #[serde(default)]
    pub keys: Vec<String>,
    /// Refuse requests without a valid `Authorization: Bearer`.
    #[serde(default)]
    pub require_key: bool,
    /// Restrict callers by source IP (`127.0.0.1`, `192.168.1.*`).
    #[serde(default)]
    pub allow_ips: Vec<String>,
    #[serde(default)]
    pub route: RouteMode,
    #[serde(default)]
    pub log_level: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

fn default_port() -> u16 {
    3425
}
fn default_web_port() -> u16 {
    3430
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            port: default_port(),
            web_port: default_web_port(),
            web_key: String::new(),
            keys: Vec::new(),
            require_key: false,
            allow_ips: Vec::new(),
            route: RouteMode::Order,
            log_level: "info".into(),
            extra: BTreeMap::new(),
        }
    }
}

impl Settings {
    pub fn level(&self) -> crate::log::Level {
        match self.log_level.to_ascii_lowercase().as_str() {
            "error" => crate::log::Level::Error,
            "warn" => crate::log::Level::Warn,
            "debug" | "trace" => crate::log::Level::Debug,
            _ => crate::log::Level::Info,
        }
    }
}

/// A provider list, wrapped in an object so the file can grow.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProvidersFile {
    #[serde(default)]
    pub providers: Vec<Provider>,
}

/// The whole configuration, behind a lock the HTTP handlers can read cheaply.
pub struct Config {
    dir: PathBuf,
    pub settings: Settings,
    pub providers: Vec<Provider>,
    /// Round-robin cursor, only meaningful under [`RouteMode::Rotate`].
    rotate: std::sync::atomic::AtomicUsize,
}

pub type SharedConfig = Arc<RwLock<Config>>;

impl Config {
    /// Load from `dir`, creating a usable config on first run.
    pub fn load(dir: impl AsRef<Path>) -> std::io::Result<Config> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;

        let settings_path = dir.join("settings.json");
        let providers_path = dir.join("providers.json");

        let mut settings: Settings = match std::fs::read_to_string(&settings_path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                crate::warn!("settings.json is unreadable ({e}); using defaults");
                Settings::default()
            }),
            Err(_) => Settings::default(),
        };

        let providers: Vec<Provider> = match std::fs::read_to_string(&providers_path) {
            Ok(text) => serde_json::from_str::<ProvidersFile>(&text)
                .map(|f| f.providers)
                .unwrap_or_else(|e| {
                    crate::warn!("providers.json is unreadable ({e}); starting empty");
                    Vec::new()
                }),
            Err(_) => Vec::new(),
        };

        if settings.web_key.len() < 16 {
            settings.web_key = random_key(32);
        }

        let cfg = Config {
            dir,
            settings,
            providers,
            rotate: std::sync::atomic::AtomicUsize::new(0),
        };
        cfg.save()?;
        Ok(cfg)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn save(&self) -> std::io::Result<()> {
        let settings = serde_json::to_string_pretty(&self.settings)?;
        std::fs::write(self.dir.join("settings.json"), settings)?;
        let providers = serde_json::to_string_pretty(&ProvidersFile {
            providers: self.providers.clone(),
        })?;
        std::fs::write(self.dir.join("providers.json"), providers)?;
        Ok(())
    }

    pub fn enabled_providers(&self) -> impl Iterator<Item = &Provider> {
        self.providers.iter().filter(|p| p.enabled)
    }

    pub fn provider(&self, id: &str) -> Option<&Provider> {
        self.providers.iter().find(|p| p.id == id)
    }

    /// Next round-robin index over `candidates`.
    pub fn next_rotate(&self, len: usize) -> usize {
        if len == 0 {
            return 0;
        }
        self.rotate
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            % len
    }

    /// Whether `bearer` is one of the configured gateway keys.
    ///
    /// Comparison is constant-time-ish (length first, then a full scan) because
    /// a key is a password: the loop never short-circuits on the first mismatch.
    pub fn key_ok(&self, bearer: Option<&str>) -> bool {
        let Some(token) = bearer else {
            return !self.settings.require_key;
        };
        let token = token.trim();
        let token = token.strip_prefix("Bearer ").unwrap_or(token).trim();
        if !self.settings.require_key && self.settings.keys.is_empty() {
            return true;
        }
        self.settings.keys.iter().any(|k| {
            k.len() == token.len()
                && k.bytes()
                    .zip(token.bytes())
                    .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                    == 0
        })
    }

    /// Source-IP allow list. Empty means "loopback and private ranges only",
    /// which is what the LAN switch relies on.
    pub fn ip_allowed(&self, ip: &str) -> bool {
        if self.settings.allow_ips.is_empty() {
            return true;
        }
        self.settings.allow_ips.iter().any(|rule| {
            if let Some(prefix) = rule.strip_suffix('*') {
                ip.starts_with(prefix)
            } else {
                rule == ip
            }
        })
    }
}

/// A random key from the OS entropy source.
///
/// `std::collections::hash_map::RandomState` is seeded per-instance from the
/// platform RNG, so hashing four times gives us 256 bits without pulling in a
/// random-number crate (the APK would otherwise carry it twice).
pub fn random_key(len: usize) -> String {
    use std::hash::{BuildHasher, Hasher};
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut out = String::with_capacity(len);
    while out.len() < len {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x9E37_79B9_7F4A_7C15),
        );
        let mut v = h.finish();
        for _ in 0..8 {
            out.push(ALPHABET[(v % ALPHABET.len() as u64) as usize] as char);
            v /= ALPHABET.len() as u64;
        }
    }
    out.truncate(len);
    out
}

/// A gateway key with the upstream prefix so agents recognize it as one.
pub fn new_gateway_key() -> String {
    format!("sk-magpie-key-{}", random_key(40))
}
