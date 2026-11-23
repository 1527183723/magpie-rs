//! Operating the page's `WebView` from Rust.
//!
//! The `WebView` object itself is created by `MainActivity` — instantiating a
//! platform view class is the one thing JNI makes genuinely painful — and handed
//! here as a global reference. Everything afterwards (which URL it loads, what
//! gets injected, when it reloads) is decided in Rust.

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

fn with_view(f: impl FnOnce(&mut jni::JNIEnv, &GlobalRef) -> Result<(), jni::errors::Error>) {
    let view = WEBVIEW.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(view) = view else { return };
    let _ = crate::jvm::with_env(|env| f(env, &view));
}

/// Load the page. Called on start and whenever the gateway restarts on a new
/// port.
pub fn load_page() {
    let url = page_url();
    with_view(|env, view| {
        let s = env.new_string(&url)?;
        env.call_method(
            view.as_obj(),
            "loadUrl",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&s)],
        )?;
        Ok(())
    });
    magpie_core::info!("page: {url}");
}

/// Show a self-contained HTML document in the WebView.
///
/// Used to put a readable failure on screen when the page server never came up:
/// a blank WebView is indistinguishable from a hang, and the phone usually has
/// no adb attached to read the log from.
pub fn load_html(html: &str) {
    let doc = html.to_string();
    with_view(move |env, view| {
        let s = env.new_string(&doc)?;
        let mime = env.new_string("text/html")?;
        let encoding = env.new_string("utf-8")?;
        let null = JObject::null();
        env.call_method(
            view.as_obj(),
            "loadDataWithBaseURL",
            "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
            &[
                JValue::Object(&null),
                JValue::Object(&s),
                JValue::Object(&mime),
                JValue::Object(&encoding),
                JValue::Object(&null),
            ],
        )?;
        Ok(())
    });
}

/// Show a toast through the activity.
pub fn toast(message: &str) {
    let Some(activity) = activity() else { return };
    let msg = message.to_string();
    let _ = crate::jvm::with_env(|env| {
        let s = env.new_string(&msg)?;
        let toast = env.call_static_method(
            "android/widget/Toast",
            "makeText",
            "(Landroid/content/Context;Ljava/lang/CharSequence;I)Landroid/widget/Toast;",
            &[
                JValue::Object(activity.as_obj()),
                JValue::Object(&s),
                JValue::Int(0),
            ],
        )?;
        let obj = toast.l()?;
        env.call_method(&obj, "show", "()V", &[])?;
        Ok(())
    });
}

pub fn page_url() -> String {
    crate::rt::page_url()
}
