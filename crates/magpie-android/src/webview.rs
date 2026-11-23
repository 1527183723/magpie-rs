//! Operating the page's `WebView` from Rust.
//!
//! The `WebView` object itself is created by `MainActivity` — instantiating a
//! platform view class is the one thing JNI makes genuinely painful — and handed
//! here as a global reference.
//!
//! Every call is routed through `io.magpie.rs.WebOps`, which hops to the main
//! Looper. Calling a `WebView` method directly from a Rust thread is not merely
//! discouraged: `checkThread` throws a `RuntimeException` that nothing on that
//! thread catches, and the process dies. That is exactly what happened in
//! v1.0.0/v1.0.1 (`WebView.java:2669 → loadUrl(WebView.java:747)`).

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

fn view() -> Option<GlobalRef> {
    WEBVIEW.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Call `io.magpie.rs.WebOps.<method>(Activity, WebView, String)`.
fn call(method: &str, arg: &str) -> Result<(), String> {
    let (Some(activity), Some(view)) = (activity(), view()) else {
        return Err("no activity or webview yet".into());
    };
    crate::jvm::with_env(|env| {
        let s = env.new_string(arg)?;
        env.call_static_method(
            "io/magpie/rs/WebOps",
            method,
            "(Landroid/app/Activity;Landroid/webkit/WebView;Ljava/lang/String;)V",
            &[
                JValue::Object(activity.as_obj()),
                JValue::Object(view.as_obj()),
                JValue::Object(&s),
            ],
        )?;
        Ok(())
    })
}

/// Load the page. Called once the page port answers.
pub fn load_page() {
    let url = page_url();
    match call("loadUrl", &url) {
        Ok(()) => magpie_core::info!("page: {url}"),
        Err(e) => magpie_core::error!("cannot load the page: {e}"),
    }
}

/// Render a self-contained document — used for the "it did not start" screen.
pub fn load_html(html: &str) {
    if let Err(e) = call("loadHtml", html) {
        magpie_core::error!("cannot render the failure page: {e}");
    }
}

pub fn toast(message: &str) {
    if let Err(e) = call("toast", message) {
        magpie_core::info!("(toast skipped: {e}) {message}");
    }
}

pub fn page_url() -> String {
    crate::rt::page_url()
}
