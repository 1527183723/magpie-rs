//! The gateway's lifetime inside the app process.
//!
//! The Go original ran the gateway as a child process and watched its exit code.
//! Rust does not need a subprocess: one tokio runtime owns the gateway, the
//! foreground service merely keeps the process alive, and a `watch` channel is
//! the stop signal that both the notification's "Stop" action and the Activity's
//! `onDestroy` send.

use magpie_core::gateway::{Gateway, GatewayOptions};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Mutex, OnceLock};

struct Inner {
    runtime: tokio::runtime::Runtime,
    gateway: std::sync::Arc<Gateway>,
    stop: tokio::sync::watch::Sender<()>,
}

static INNER: OnceLock<Mutex<Option<Inner>>> = OnceLock::new();
static RUNNING: AtomicBool = AtomicBool::new(false);
static PORT: AtomicU16 = AtomicU16::new(3425);
static WEB_PORT: AtomicU16 = AtomicU16::new(3430);
static WEB_KEY: OnceLock<String> = OnceLock::new();

fn slot() -> &'static Mutex<Option<Inner>> {
    INNER.get_or_init(|| Mutex::new(None))
}

pub fn is_running() -> bool {
    RUNNING.load(Ordering::Relaxed)
}

pub fn gateway_port() -> u16 {
    PORT.load(Ordering::Relaxed)
}

pub fn web_port() -> u16 {
    WEB_PORT.load(Ordering::Relaxed)
}

pub fn page_url() -> String {
    let key = WEB_KEY.get().cloned().unwrap_or_default();
    format!("http://127.0.0.1:{}/?k={}", web_port(), key)
}

pub fn gateway_url() -> String {
    format!("http://127.0.0.1:{}/v1", gateway_port())
}

/// Start the gateway unless it is already up.
///
/// `files_dir` is the app's private data directory; everything magpie writes
/// (config, log, usage) stays under it, exactly the layout the Go build used
/// through `HOME` and the XDG variables.
pub fn start(files_dir: &Path, lan: bool, gateway_port: u16, web_port: u16) -> Result<(), String> {
    if is_running() {
        return Ok(());
    }
    let config_dir = files_dir.join("config");
    std::fs::create_dir_all(&config_dir)
        .map_err(|e| format!("cannot create {config_dir:?}: {e}"))?;

    let addr = if lan {
        format!("0.0.0.0:{gateway_port}")
    } else {
        format!("127.0.0.1:{gateway_port}")
    };

    let opts = GatewayOptions {
        config_dir: config_dir.clone(),
        gateway_addr: addr.clone(),
        // The page always stays on loopback: it can show and change every API
        // key, so it is not the thing to put on a network.
        web_addr: Some(format!("127.0.0.1:{web_port}")),
        web_key: None,
        log_path: Some(config_dir.join("magpie.log")),
        echo_log: false,
        quiet: false,
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("magpie")
        .build()
        .map_err(|e| format!("tokio: {e}"))?;

    let gateway = runtime
        .block_on(async { Gateway::new(opts) })
        .map_err(|e| format!("gateway: {e}"))?;

    // Remember the effective ports and key so the WebView and the notification
    // agree with what actually got bound.
    let (gp, wp, key) = {
        let cfg = gateway.cfg.read().unwrap_or_else(|e| e.into_inner());
        (
            cfg.settings.port,
            cfg.settings.web_port,
            cfg.settings.web_key.clone(),
        )
    };
    PORT.store(
        if gateway_port == 0 { gp } else { gateway_port },
        Ordering::Relaxed,
    );
    WEB_PORT.store(if web_port == 0 { wp } else { web_port }, Ordering::Relaxed);
    let _ = WEB_KEY.set(key);

    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(());
    let gw = gateway.clone();
    runtime.spawn(async move {
        // A bind failure used to be swallowed here, which left the WebView
        // pointing at a port nothing was listening on.
        if let Err(e) = gw
            .run(async move {
                let _ = stop_rx.changed().await;
            })
            .await
        {
            magpie_core::error!("gateway stopped: {e}");
        }
    });

    *slot().lock().unwrap_or_else(|e| e.into_inner()) = Some(Inner {
        runtime,
        gateway,
        stop: stop_tx,
    });
    RUNNING.store(true, Ordering::Relaxed);
    magpie_core::info!(
        "gateway up on {addr}, page on 127.0.0.1:{}",
        WEB_PORT.load(Ordering::Relaxed)
    );
    Ok(())
}

pub fn stop() {
    RUNNING.store(false, Ordering::Relaxed);
    let inner = slot().lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(inner) = inner {
        let _ = inner.stop.send(());
        // Give the graceful shutdown a moment, then drop the runtime, which
        // cancels anything still running.
        inner
            .runtime
            .block_on(async { tokio::time::sleep(std::time::Duration::from_millis(150)).await });
        inner
            .runtime
            .shutdown_timeout(std::time::Duration::from_secs(1));
        magpie_core::info!("gateway stopped");
    }
}

pub fn gateway() -> Option<std::sync::Arc<Gateway>> {
    slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|i| i.gateway.clone())
}
