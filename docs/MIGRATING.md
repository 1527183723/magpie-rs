# 迁移笔记

## `jni` 0.21 → 0.22

本仓库钉在 `jni = "0.21"`。这不是"忘了升级"：0.22 是一次 API 重塑，而 Android 生态里的绝大多数代码、文档和示例还在 0.21 的形状上。

### 0.22 改了什么

| 0.21 | 0.22 | 影响 |
|---|---|---|
| `JNIEnv<'local>` | `Env<'local>`，native 入口是 `EnvUnowned<'local>` | 每个导出的 JNI 函数签名都要改，而且不能直接对 `EnvUnowned` 调方法 |
| `vm.attach_current_thread() -> Result<JNIEnv>` | `vm.attach_current_thread(|env: &mut Env| -> Result<T, E>)` | 回调式：所有"拿到 env 做点事"的辅助函数都要重写成闭包 |
| `GlobalRef<'local>` | `GlobalRef<T>`（`Global<T>` 的别名） | 静态字段要写成 `GlobalRef<JObject<'static>>`，`new_global_ref` 的返回类型跟着 `O::GlobalKind` 走 |
| `env.new_string(..)` 在 `JNIEnv` 上 | 在 `Env` 上 | 方法名不变，接收者类型变了 |

### 升级步骤

1. `Cargo.toml` 把 `jni` 改成 `"0.22"`。
2. `jni::JNIEnv` → `jni::Env`；所有 native 入口的第一参数改成 `jni::objects::JObject`（0.22 里 `EnvUnowned` 需要先 `with_env` 才能拿到 `Env`，而本项目每个入口都用 `attach_current_thread` 自己取）。
3. `jvm::with_env` 从"attach 后返回 guard"改成"把闭包交给 `attach_current_thread`"：

   ```rust
   // 0.21
   let mut guard = vm.attach_current_thread()?;
   f(&mut guard)
   // 0.22
   vm.attach_current_thread(|env| f(env))
   ```

4. `static WEBVIEW: Mutex<Option<GlobalRef>>` → `Mutex<Option<GlobalRef<JObject<'static>>>>`。
5. `webview::eval` 目前被删掉了（没有调用方）；恢复时记得 `evaluateJavascript(String, ValueCallback)` 的第二个参数要传 `JObject::null()`。

改完之后 `cargo check -p magpie-android` 会指出剩下所有位置——0.22 的编译期检查足够严格，不会有静默行为变化。

## Android SDK / NDK 版本

`build-apk.ps1` 里写死了 `platforms\android-36` 和 `build-tools\37.0.0`。换版本只要改脚本顶部三个变量（`$androidJar`、`$buildTools`、`$apiLevel`），manifest 不用动。

NDK 换了之后注意：`<triple><API>-clang.cmd` 的 API 号上限随 NDK 版本变化（r30 到 37，r28 到 35）。脚本会在 `.cmd` 不存在时退回不带 API 号的 wrapper，所以换成更老的 NDK 也能编，只是 API level 由 NDK 自己挑。

## 上游 magpie 跟进

`yetone/magpie` 还在动。要跟进它的新端点或新供应商：

1. 先看它是不是新协议。如果是，在 `crates/magpie-core/src/protocol/` 加一个模块，并在 `ir.rs` 里补上需要双向转换的字段。
2. 如果是同一协议的新端点（比如 `/v1/embeddings`），在 `gateway.rs` 的 `api_router` 里加路由，体处理复用 `call_upstream`。
3. 跑 `cargo test -p magpie-core` —— `tests/translation.rs` 里的往返测试是防止"加了字段忘了另一边"的唯一保障。
