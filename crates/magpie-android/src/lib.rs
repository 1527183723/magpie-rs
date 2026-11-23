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

fn catch<T>(what: &str, f: impl FnOnce() -> T + std::panic::UnwindSafe, fallback: T) -> T {
    match std::panic::catch_unwind(f) {
        Ok(v) => v,
        Err(_) => {
            magpie_core::error!("{what} panicked");
            fallback
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

/// Wait for the portal to accept connections, then point the WebView at it.
///
/// Loading before the page port is bound shows the platform's "webpage not
/// available" screen, which looks like a crash to the user.
fn wait_and_load() {
    std::thread::spawn(move || {
        for _ in 0..150 {
            if rt::is_running() && port_open(rt::web_port()) {
                webview::load_page();
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        magpie_core::error!("the page port never opened");
        webview::load_page();
    });
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
        "magpie-rs {} (versionCode {}, API {}) starting",
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
    catch("nativeStop", rt::stop, ());
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
        let result = catch(
            "update check",
            update::latest,
            Err(update::UpdateError::NoRelease),
        );
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
                catch("update download", download_and_install, ());
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
    catch("service stop", rt::stop, ());
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_magpie_rs_MagpieService_nativeRunning(
    _env: JNIEnv,
    _this: JObject,
) -> jboolean {
    rt::is_running() as jboolean
}

// ------------------------------------------------- java-side UI, called by us

/// `io.magpie.rs.ShellUi.showUpdate(Activity, version, size, notes)`.
fn show_update_dialog(release: &update::Release) {
    let _ = jvm::with_env(|env| {
        let Some(activity) = webview::activity() else {
            return Ok(());
        };
        let version = env.new_string(&release.tag)?;
        let size = env.new_string(update::size_text(release))?;
        let notes = env.new_string(first_lines(&release.notes, 12))?;
        env.call_static_method(
            "io/magpie/rs/ShellUi",
            "showUpdate",
            "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
            &[
                jni::objects::JValue::Object(activity.as_obj()),
                jni::objects::JValue::Object(&version),
                jni::objects::JValue::Object(&size),
                jni::objects::JValue::Object(&notes),
            ],
        )?;
        Ok(())
    });
}

fn update_progress(pct: u8) {
    let _ = jvm::with_env(|env| {
        let Some(activity) = webview::activity() else {
            return Ok(());
        };
        env.call_static_method(
            "io/magpie/rs/ShellUi",
            "updateProgress",
            "(Landroid/content/Context;I)V",
            &[
                jni::objects::JValue::Object(activity.as_obj()),
                jni::objects::JValue::Int(pct as jint),
            ],
        )?;
        Ok(())
    });
}

fn install_apk(path: &std::path::Path) {
    let p = path.display().to_string();
    let _ = jvm::with_env(|env| {
        let Some(activity) = webview::activity() else {
            return Ok(());
        };
        let s = env.new_string(&p)?;
        env.call_static_method(
            "io/magpie/rs/ApkInstaller",
            "install",
            "(Landroid/content/Context;Ljava/lang/String;)V",
            &[
                jni::objects::JValue::Object(activity.as_obj()),
                jni::objects::JValue::Object(&s),
            ],
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
