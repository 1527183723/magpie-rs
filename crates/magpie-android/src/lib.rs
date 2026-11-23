//! magpie-rs for Android — the native half of the shell.
//!
//! The Java side is deliberately thin: `MainActivity` builds one `WebView` and
//! hands it over, `MagpieService` keeps the process alive, `BootReceiver` starts
//! the service after a reboot, and `ApkInstaller` runs the platform install
//! session. Every decision — which port, which page, whether to update, what to
//! do with the reply — happens in this library.

mod jvm;
mod rt;
mod shell;
mod update;
mod webview;

use jni::JNIEnv;
use jni::objects::{JObject, JString};
use jni::sys::{JNI_VERSION_1_6, jboolean, jint, jlong, jstring};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static UPDATE_BUSY: AtomicBool = AtomicBool::new(false);
/// Guards against two entries both starting the page loader: `nativeStart` and
/// `nativeAttachWebView` each used to spawn one, so every failure was reported
/// twice and every load raced itself.
static LOADER_STARTED: AtomicBool = AtomicBool::new(false);

/// Run `f`, turning a panic into a logged error instead of an abort.
///
/// `AssertUnwindSafe` is deliberate: every closure here is a one-shot JNI entry
/// point whose captured state (a `&mut JNIEnv`, a path) is not used again if it
/// unwinds, so the usual objection to catching across a `&mut` does not apply.
fn catch<T>(what: &str, f: impl FnOnce() -> T) -> Option<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(v) => Some(v),
        Err(payload) => {
            let detail = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            magpie_core::error!("{what} panicked: {detail}");
            None
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn JNI_OnLoad(vm: jni::JavaVM, _reserved: *mut std::ffi::c_void) -> jint {
    jvm::set(vm);
    JNI_VERSION_1_6
}

fn port_open(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(150),
    )
    .is_ok()
}

/// Wait for the page port to accept connections, then point the WebView at it.
///
/// Loading before the port is bound shows the platform's "webpage not
/// available" screen; never loading leaves a black window that looks identical
/// to a crash. If the port does not come up, the log tail is rendered instead so
/// the failure is readable without adb.
fn wait_and_load() {
    if LOADER_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        for _ in 0..150 {
            if rt::is_running() && webview::has_view() && port_open(rt::web_port()) {
                webview::load_page();
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        magpie_core::error!("the page port never opened");
        webview::load_html(&failure_page());
    });
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A readable "it did not start" page: what the shell knows, then the log.
fn failure_page() -> String {
    let lines = magpie_core::log::tail(60)
        .iter()
        .map(|l| html_escape(l))
        .collect::<Vec<_>>()
        .join("\n");
    let gateway_port = rt::gateway_port();
    let web_port = rt::web_port();
    format!(
        r#"<!doctype html><meta name=viewport content="width=device-width,initial-scale=1">
<style>
 body{{font:14px ui-monospace,Menlo,monospace;background:#0e1116;color:#e6edf3;margin:0;padding:18px}}
 h3{{color:#f85149;font:600 15px system-ui;margin:0 0 10px}}
 dt{{color:#8b949e;font:12px system-ui;margin-top:8px}}
 dd{{margin:2px 0 0;word-break:break-all}}
 pre{{background:#05070a;border:1px solid #2a323d;border-radius:10px;padding:10px;
      white-space:pre-wrap;font-size:11.5px;line-height:1.5;margin-top:14px}}
</style>
<h3>magpie 网关没有起来</h3>
<dl>
 <dt>网关端口</dt><dd>127.0.0.1:{gateway_port}</dd>
 <dt>页面端口</dt><dd>127.0.0.1:{web_port}</dd>
 <dt>说明</dt><dd>端口可能被占用，或启动过程中出错。下面是本次启动的日志。</dd>
</dl>
<pre>{lines}</pre>"#
    )
}

fn start_gateway(files_dir: &str) {
    let settings = shell::settings();
    if let Err(e) = rt::start(
        std::path::Path::new(files_dir),
        settings.lan,
        settings.gateway_port,
        settings.web_port,
    ) {
        magpie_core::error!("gateway did not start: {e}");
        webview::toast(&format!("magpie did not start: {e}"));
    }
}

// ---------------------------------------------------------------- MainActivity

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_magpie_rs_MainActivity_nativeStart(
    mut env: JNIEnv,
    this: JObject,
    files_dir: JString,
    version_name: JString,
    version_code: jlong,
    sdk: jint,
) {
    // Everything is inside `catch`: a panic in an `extern "system"` function
    // cannot unwind, so without this a single bad line here aborts the process
    // and the user just sees the launcher again.
    catch("nativeStart", || {
        let dir = jvm::jstring(&mut env, &files_dir);
        let version = jvm::jstring(&mut env, &version_name);
        let _ = webview::attach_activity(&mut env, &this);
        shell::set_files_dir(std::path::PathBuf::from(&dir));

        magpie_core::log::init(
            Some(&std::path::Path::new(&dir).join("config").join("magpie.log")),
            false,
            magpie_core::log::Level::Info,
        );
        magpie_core::info!(
            "magpie-rs {} (versionCode {}, API {}) starting; files={dir}",
            env!("CARGO_PKG_VERSION"),
            version_code,
            sdk
        );
        let _ = version;

        let dir2 = dir.clone();
        std::thread::spawn(move || {
            start_gateway(&dir2);
            wait_and_load();
        });
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_magpie_rs_MainActivity_nativeAttachWebView(
    mut env: JNIEnv,
    _this: JObject,
    view: JObject,
) {
    if let Err(e) = webview::attach_view(&mut env, &view) {
        magpie_core::error!("cannot hold the WebView: {e}");
        return;
    }
    wait_and_load();
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_magpie_rs_MainActivity_nativeStop(_env: JNIEnv, _this: JObject) {
    catch("nativeStop", rt::stop);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_magpie_rs_MainActivity_nativeOpenPage(_env: JNIEnv, _this: JObject) {
    webview::load_page();
}

/// A one-object snapshot for the Activity's own toolbar text.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_magpie_rs_MainActivity_nativeStatus(
    env: JNIEnv,
    _this: JObject,
) -> jstring {
    let (requests, errors) = rt::gateway().map(|g| g.counters()).unwrap_or((0, 0));
    let value = serde_json::json!({
        "running": rt::is_running(),
        "gateway_port": rt::gateway_port(),
        "web_port": rt::web_port(),
        "page_url": rt::page_url(),
        "gateway_url": rt::gateway_url(),
        "requests": requests,
        "errors": errors,
        "version": env!("CARGO_PKG_VERSION"),
    })
    .to_string();
    match env.new_string(value) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

// -------------------------------------------------------------- in-app update

fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn with_ui(message: &str) {
    webview::toast(message);
}

/// Ask GitHub what the newest release is. `silent` suppresses the "you are up to
/// date" toast so the automatic check on start-up stays quiet.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_magpie_rs_MainActivity_nativeCheckUpdate(
    _env: JNIEnv,
    _this: JObject,
    silent: jboolean,
) {
    let silent = silent != 0;
    if UPDATE_BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let result =
            catch("update check", update::latest).unwrap_or(Err(update::UpdateError::NoRelease));
        UPDATE_BUSY.store(false, Ordering::SeqCst);
        match result {
            Ok(release) => {
                let current = current_version();
                if !update::is_newer(&current, &release.tag) {
                    magpie_core::info!("up to date ({} vs {})", current, release.tag);
                    if !silent {
                        with_ui("magpie is up to date");
                    }
                    return;
                }
                if silent && shell::settings().skipped_version == release.tag {
                    return;
                }
                magpie_core::info!("update available: {} -> {}", current, release.tag);
                crate::show_update_dialog(&release);
            }
            Err(e) => {
                magpie_core::warn!("update check failed: {e}");
                if !silent {
                    with_ui(&format!("update check: {e}"));
                }
            }
        }
    });
}

/// The user's answer from the dialog: 1 download, 2 later, 3 skip this version.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_magpie_rs_MainActivity_nativeUpdateChoice(
    _env: JNIEnv,
    _this: JObject,
    choice: jint,
) {
    match choice {
        1 => {
            std::thread::spawn(|| {
                catch("update download", download_and_install);
            });
        }
        3 => {
            std::thread::spawn(|| {
                let Ok(release) = update::latest() else {
                    return;
                };
                let Some(dir) = shell::files_dir() else {
                    return;
                };
                let mut s = shell::load(dir);
                s.skipped_version = release.tag.clone();
                let _ = shell::save(dir, &s);
                magpie_core::info!("update {} will not be offered again", release.tag);
            });
        }
        _ => {}
    }
}

fn download_and_install() {
    let Ok(release) = update::latest() else {
        with_ui("cannot reach GitHub");
        return;
    };
    let Some(dir) = shell::files_dir() else {
        return;
    };
    let target = dir.join("update");
    magpie_core::info!("downloading {}", release.apk_url);
    match update::download(&release, &target, |pct| {
        crate::update_progress(pct);
    }) {
        Ok(path) => {
            magpie_core::info!("installing {}", path.display());
            crate::install_apk(&path);
        }
        Err(e) => {
            magpie_core::error!("download failed: {e}");
            with_ui(&format!("download failed: {e}"));
        }
    }
}

// --------------------------------------------------------------- MagpieService

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_magpie_rs_MagpieService_nativeEnsureRunning(
    mut env: JNIEnv,
    _this: JObject,
    files_dir: JString,
) {
    let dir = jvm::jstring(&mut env, &files_dir);
    shell::set_files_dir(std::path::PathBuf::from(&dir));
    if rt::is_running() {
        return;
    }
    std::thread::spawn(move || start_gateway(&dir));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_magpie_rs_MagpieService_nativeStop(_env: JNIEnv, _this: JObject) {
    catch("service stop", rt::stop);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_magpie_rs_MagpieService_nativeRunning(
    _env: JNIEnv,
    _this: JObject,
) -> jboolean {
    rt::is_running() as jboolean
}

// ------------------------------------------------- java-side UI, called by us
//
// All of these call an *instance* method on the Activity, never
// `CallStaticVoidMethod` with a class name. A natively attached thread has no
// Java frame, so `FindClass("io/magpie/rs/ShellUi")` resolves through the system
// class loader and fails — the class is in the APK but invisible from here.
// `GetObjectClass(activity)` has no such problem.

fn with_activity<R>(
    f: impl FnOnce(&mut jni::JNIEnv, &jni::objects::GlobalRef) -> Result<R, jni::errors::Error>,
) -> Option<R> {
    let activity = webview::activity()?;
    match jvm::with_env(|env| f(env, &activity)) {
        Ok(v) => Some(v),
        Err(e) => {
            magpie_core::error!("call into MainActivity failed: {e}");
            None
        }
    }
}

/// `MainActivity.magpieShowUpdate(version, size, notes)`.
fn show_update_dialog(release: &update::Release) {
    let notes = first_lines(&release.notes, 12);
    with_activity(|env, activity| {
        let version = env.new_string(&release.tag)?;
        let size = env.new_string(update::size_text(release))?;
        let notes = env.new_string(&notes)?;
        env.call_method(
            activity.as_obj(),
            "magpieShowUpdate",
            "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
            &[
                jni::objects::JValue::Object(&version),
                jni::objects::JValue::Object(&size),
                jni::objects::JValue::Object(&notes),
            ],
        )?;
        Ok(())
    });
}

fn update_progress(pct: u8) {
    with_activity(|env, activity| {
        env.call_method(
            activity.as_obj(),
            "magpieUpdateProgress",
            "(I)V",
            &[jni::objects::JValue::Int(pct as jint)],
        )?;
        Ok(())
    });
}

fn install_apk(path: &std::path::Path) {
    let p = path.display().to_string();
    with_activity(|env, activity| {
        let s = env.new_string(&p)?;
        env.call_method(
            activity.as_obj(),
            "magpieInstall",
            "(Ljava/lang/String;)V",
            &[jni::objects::JValue::Object(&s)],
        )?;
        Ok(())
    });
}

fn first_lines(text: &str, n: usize) -> String {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .take(n)
        .collect::<Vec<_>>()
        .join("\n")
}
