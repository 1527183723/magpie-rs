//! The JVM handle and the few helpers every JNI entry point needs.
//!
//! `android-activity`/`ndk-context` are intentionally not used: the shell is a
//! plain `Activity` holding a `WebView`, so the only thing Rust needs from the
//! platform is the `JavaVM` it is handed in `JNI_OnLoad`.

use jni::JavaVM;
use std::sync::OnceLock;

static VM: OnceLock<JavaVM> = OnceLock::new();

pub fn set(vm: JavaVM) {
    let _ = VM.set(vm);
}

pub fn vm() -> Option<&'static JavaVM> {
    VM.get()
}

/// Run `f` with an attached `JNIEnv` on the current thread.
///
/// Attaching per call is cheap on Android after the first time, and it keeps the
/// shell free of a thread-attach/ detach dance around every callback.
pub fn with_env<R>(
    f: impl FnOnce(&mut jni::JNIEnv) -> Result<R, jni::errors::Error>,
) -> Result<R, String> {
    let vm = vm().ok_or("the JVM is not attached yet")?;
    let mut guard = vm
        .attach_current_thread()
        .map_err(|e| format!("attach: {e}"))?;
    f(&mut guard).map_err(|e| format!("jni: {e}"))
}

/// Translate a Java `String` into a Rust `String`, tolerating null.
pub fn jstring(env: &mut jni::JNIEnv, s: &jni::objects::JString) -> String {
    if s.is_null() {
        return String::new();
    }
    env.get_string(s)
        .map(|v| v.into())
        .unwrap_or_else(|_| String::new())
}
