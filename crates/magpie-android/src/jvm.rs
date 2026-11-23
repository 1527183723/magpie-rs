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

/// Run `f` with an attached `JNIEnv` on the current thread, and make sure no JNI
/// exception is left pending when it returns.
///
/// That last part is not politeness. A pending exception thrown by a `FindClass`
/// or a `CallVoidMethod` on a natively attached thread has no Java frame to
/// unwind into, so it reaches ART and aborts the process. v1.0.2 died exactly
/// that way: `ClassNotFoundException: io.magpie.rs.WebOps`, no Java stack, no
/// chance to log anything.
pub fn with_env<R>(
    f: impl FnOnce(&mut jni::JNIEnv) -> Result<R, jni::errors::Error>,
) -> Result<R, String> {
    let vm = vm().ok_or("the JVM is not attached yet")?;
    let mut guard = vm
        .attach_current_thread()
        .map_err(|e| format!("attach: {e}"))?;

    let out = f(&mut guard);

    // `exception_describe` writes the pending exception to logcat before it is
    // dropped, which is the only record of it that survives.
    if let Ok(true) = guard.exception_check() {
        let _ = guard.exception_describe();
        let _ = guard.exception_clear();
    }

    out.map_err(|e| format!("jni: {e}"))
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
