//! Operating the page from Rust.
//!
//! Two rules govern this file, both learned from crashes on a real device:
//!
//! 1. **Never `FindClass` an app class from a natively attached thread.** Such a
//!    thread has no Java frame, so `FindClass` falls back to the system class
//!    loader, which can see `android.webkit.WebView` but *not*
//!    `io.magpie.rs.Anything`. v1.0.2 died on exactly that:
//!    `ClassNotFoundException: io.magpie.rs.WebOps`.
//!
//!    The fix is to hold the `Activity` (already a `GlobalRef`) and call an
//!    *instance* method on it. `CallObjectMethod` resolves the method through
//!    `GetObjectClass(obj)`, which needs no class lookup at all.
//!
//! 2. **Never leave a JNI exception pending.** The Java side does the thread hop
//!    (`runOnUiThread`); if anything throws there it is caught in Java. If the
//!    JNI call itself fails, `jvm::with_env` clears the exception before it can
//!    reach ART.

use jni::objects::{GlobalRef, JObject, JValue};
use std::sync::Mutex;

static WEBVIEW: Mutex<Option<GlobalRef>> = Mutex::new(None);
static ACTIVITY: Mutex<Option<GlobalRef>> = Mutex::new(None);

pub fn attach_view(env: &mut jni::JNIEnv, view: &JObject) -> Result<(), String> {
    let global = env
        .new_global_ref(view)
        .map_err(|e| format!("webview ref: {e}"))?;
    *WEBVIEW.lock().unwrap_or_else(|e| e.into_inner()) = Some(global);
    Ok(())
}

pub fn attach_activity(env: &mut jni::JNIEnv, activity: &JObject) -> Result<(), String> {
    let global = env
        .new_global_ref(activity)
        .map_err(|e| format!("activity ref: {e}"))?;
    *ACTIVITY.lock().unwrap_or_else(|e| e.into_inner()) = Some(global);
    Ok(())
}

pub fn activity() -> Option<GlobalRef> {
    ACTIVITY.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn has_view() -> bool {
    WEBVIEW.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

/// Call `MainActivity.magpie<Name>(String)` — resolved on the object, not looked
/// up by name in a class loader.
fn call_string(method: &str, arg: &str) -> Result<(), String> {
    let activity = activity().ok_or("the activity is not attached yet")?;
    crate::jvm::with_env(|env| {
        let s = env.new_string(arg)?;
        env.call_method(
            activity.as_obj(),
            method,
            "(Ljava/lang/String;)V",
            &[JValue::Object(&s)],
        )?;
        Ok(())
    })
}

/// Load the page. Called once the page port answers.
pub fn load_page() {
    let url = page_url();
    match call_string("magpieLoadUrl", &url) {
        Ok(()) => magpie_core::info!("page: {url}"),
        Err(e) => magpie_core::error!("cannot load the page: {e}"),
    }
}

/// Render a self-contained document — used for the "it did not start" screen.
pub fn load_html(html: &str) {
    if let Err(e) = call_string("magpieLoadHtml", html) {
        magpie_core::error!("cannot render the failure page: {e}");
    }
}

pub fn toast(message: &str) {
    if let Err(e) = call_string("magpieToast", message) {
        magpie_core::info!("(toast skipped: {e}) {message}");
    }
}

pub fn page_url() -> String {
    crate::rt::page_url()
}
