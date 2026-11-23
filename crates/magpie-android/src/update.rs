//! In-app update: ask the repository's latest release what it is, compare with
//! what is installed, download the APK, hand it to the platform installer.
//!
//! Everything except the final `PackageInstaller` session happens here. The
//! states the caller has to render are deliberately few: up to date, a newer
//! version, no releases yet, no network, and a version string we cannot parse.

use std::io::Write;
use std::path::{Path, PathBuf};

/// `OWNER/REPO`, overridable at build time so a fork can point at itself.
pub const REPO: &str = match option_env!("MAGPIE_UPDATE_REPO") {
    Some(v) => v,
    None => "1527183723/magpie-rs",
};

#[derive(Debug, Clone)]
pub struct Release {
    pub tag: String,
    pub notes: String,
    /// Direct download URL of the `.apk` asset.
    pub apk_url: String,
    pub apk_size: u64,
}

#[derive(Debug)]
pub enum UpdateError {
    /// The API answered 404: the repository exists but has no release yet.
    NoRelease,
    /// No network, DNS failure, TLS failure, timeout.
    Offline(String),
    /// Something answered, but not with a release we can use.
    BadPayload(String),
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateError::NoRelease => write!(f, "this repository has no published release yet"),
            UpdateError::Offline(e) => write!(f, "cannot reach GitHub: {e}"),
            UpdateError::BadPayload(e) => write!(f, "unexpected reply from GitHub: {e}"),
        }
    }
}

fn client() -> Result<reqwest::Client, UpdateError> {
    reqwest::Client::builder()
        .user_agent(concat!("magpie-rs-android/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| UpdateError::Offline(e.to_string()))
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime always builds")
        .block_on(f)
}

/// `v1.2.3` and `1.2.3` and `1.2.3-rc1` all compare as you would hope; anything
/// that is not numeric sorts before anything that is.
pub fn parse_version(v: &str) -> Vec<u64> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let core = v.split(['-', '+']).next().unwrap_or(v);
    core.split('.')
        .map(|p| {
            let digits: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse::<u64>().unwrap_or(0)
        })
        .collect()
}

pub fn is_newer(current: &str, latest: &str) -> bool {
    let a = parse_version(current);
    let b = parse_version(latest);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if y > x {
            return true;
        }
        if y < x {
            return false;
        }
    }
    false
}

fn human(size: u64) -> String {
    if size > 1024 * 1024 {
        format!("{:.1} MB", size as f64 / (1024.0 * 1024.0))
    } else if size > 1024 {
        format!("{} KB", size / 1024)
    } else {
        format!("{size} B")
    }
}

pub fn size_text(r: &Release) -> String {
    human(r.apk_size)
}

/// `GET /repos/{REPO}/releases/latest`.
pub fn latest() -> Result<Release, UpdateError> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let client = client()?;
    let resp = block_on(async {
        client
            .get(&url)
            .header("accept", "application/vnd.github+json")
            .send()
            .await
    })
    .map_err(|e| UpdateError::Offline(e.to_string()))?;

    if resp.status().as_u16() == 404 {
        return Err(UpdateError::NoRelease);
    }
    if !resp.status().is_success() {
        return Err(UpdateError::BadPayload(format!(
            "HTTP {}",
            resp.status().as_u16()
        )));
    }
    let body =
        block_on(async { resp.text().await }).map_err(|e| UpdateError::Offline(e.to_string()))?;
    let v: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| UpdateError::BadPayload(e.to_string()))?;

    let tag = v
        .get("tag_name")
        .and_then(|x| x.as_str())
        .ok_or_else(|| UpdateError::BadPayload("no tag_name".into()))?
        .to_string();

    let assets = v
        .get("assets")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    let apk = assets
        .iter()
        .find(|a| {
            a.get("name")
                .and_then(|n| n.as_str())
                .map(|n| n.ends_with(".apk") && !n.contains("unsigned"))
                .unwrap_or(false)
        })
        .ok_or_else(|| UpdateError::BadPayload("the release carries no .apk asset".into()))?;

    Ok(Release {
        tag,
        notes: v
            .get("body")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        apk_url: apk
            .get("browser_download_url")
            .and_then(|x| x.as_str())
            .ok_or_else(|| UpdateError::BadPayload("asset has no download URL".into()))?
            .to_string(),
        apk_size: apk.get("size").and_then(|x| x.as_u64()).unwrap_or(0),
    })
}

/// Download the APK into `dir`, calling `progress(0..=100)` as it goes.
pub fn download(
    release: &Release,
    dir: &Path,
    mut progress: impl FnMut(u8),
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {dir:?}: {e}"))?;
    let dest = dir.join(format!("magpie-{}.apk", release.tag));

    // A partially downloaded file from a previous attempt is not resumable, so
    // it is dropped first: installing a truncated APK fails in a confusing way.
    let part = dest.with_extension("apk.part");
    let _ = std::fs::remove_file(&part);

    let client = client().map_err(|e| e.to_string())?;
    let resp = block_on(async { client.get(&release.apk_url).send().await })
        .map_err(|e| format!("download failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download failed: HTTP {}", resp.status().as_u16()));
    }
    let total = resp.content_length().unwrap_or(release.apk_size).max(1);

    let mut file = std::fs::File::create(&part).map_err(|e| format!("cannot write: {e}"))?;
    let mut written: u64 = 0;
    block_on(async {
        let mut stream = resp.bytes_stream();
        use futures_util::StreamExt;
        while let Some(chunk) = stream.next().await {
            let Ok(chunk) = chunk else { break };
            if file.write_all(&chunk).is_err() {
                break;
            }
            written += chunk.len() as u64;
            progress(((written as f64 / total as f64) * 100.0).min(100.0) as u8);
        }
    });

    drop(file);
    if written == 0 {
        let _ = std::fs::remove_file(&part);
        return Err("download produced no bytes".into());
    }
    std::fs::rename(&part, &dest).map_err(|e| format!("cannot finalize: {e}"))?;
    progress(100);
    magpie_core::info!("update {} downloaded: {} bytes", release.tag, written);
    Ok(dest)
}
