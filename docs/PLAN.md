# 阶段二：Rust 重构方案

前置结论见 [ANALYSIS.md](ANALYSIS.md)。核心矛盾：原项目里「壳」和「网关」的代码量差两个数量级，所以方案必须先说清楚**重构边界**，否则"保持原有功能一致"这句话没法验收。

## 0. 边界（这一节就是决策本身）

| 层 | 原实现 | 本项目 | 理由 |
|---|---|---|---|
| 安卓壳 | Java 5 个类，59 KB | **Rust 重写全部逻辑**，Java 只留平台声明 | 壳的逻辑（进程管理、配置、通知、更新）在 Rust 里更短更安全 |
| 网关 | Go，`internal/` 约 30 万行 | **Rust 重写「agent 会打到的部分」** | 详见下面「覆盖范围」 |
| 插件运行时 | Bun 子进程（176 MB） | 不做 | 见 [差异表](../README.md#与原项目的差异) |

「agent 会打到的部分」的判定标准很硬：**`base_url = http://127.0.0.1:3425/v1` 指向它之后，Claude Code / Codex / 任意 OpenAI SDK 能正常跑完一次带工具调用的对话**。凡是这条路径上用不到的（会话抓取、Library 注入、余额查询、订阅账号 OAuth、TUI），都不在范围内，并在 README 里逐条列出。

## 1. 工程结构

```
magpie-rs/
├── Cargo.toml                  # workspace, resolver 3, edition 2024
├── rust-toolchain.toml         # stable + 两个 android target
├── .cargo/config.toml          # 16 KB page alignment（见 §5）
├── crates/
│   ├── magpie-core/            # 纯 Rust，无 android 依赖
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── main.rs         # 桌面 CLI：serve / key / web / providers
│   │   │   ├── config.rs       # settings.json + providers.json
│   │   │   ├── ir.rs           # 中间表示
│   │   │   ├── log.rs          # 文件 + 内存环
│   │   │   ├── route.rs        # 模型 → 供应商
│   │   │   ├── usage.rs        # usage.jsonl
│   │   │   ├── gateway.rs      # axum 路由、鉴权、上游调用、流式
│   │   │   ├── web.rs          # 配置页 API
│   │   │   ├── web/index.html  # 单文件 UI（include_str!）
│   │   │   └── protocol/{mod,openai,anthropic,gemini}.rs
│   │   └── tests/translation.rs
│   └── magpie-android/         # cdylib → libmagpie.so
│       └── src/{lib,rt,jvm,webview,update,shell}.rs
├── android/app/src/main/       # manifest、res、8 个 Java 文件
├── scripts/build-apk.{ps1,sh}  # 不依赖 Gradle 的 7 步打包
├── docs/{ANALYSIS,PLAN,MIGRATING}.md
└── .github/workflows/release.yml
```

拆两个 crate 的唯一理由：**`magpie-core` 必须能在桌面上 `cargo run` 起来**。没有这个能力，协议翻译就只能靠"读完代码觉得对"，而这类代码恰恰是读起来对、跑起来错的典型。测试和桌面调试都建立在这条分界上。

## 2. Rust 与 Android 的集成方式

### 选型对比

| 方案 | 结论 |
|---|---|
| `cargo-apk` + `android-activity`（NativeActivity） | 纯 Rust 无 Java，但**拿不到 WebView**：`WebView` 只能从 Java 侧实例化，`NativeActivity` 没有可用的 `setContentView` 路径。要硬做就得用 JNI 反射造 `Activity` 上下文 + 手动塞 view 树 |
| Flutter/RN 式「JS 桥」 | 与主题无关 |
| **`cdylib` + 薄 Java 壳**（本项目） | Java 只做「造控件 / 收广播 / 转发回调」，全部判断在 Rust |

Android 的四大组件必须由系统实例化 `android.app.*` 的子类——这是平台约束，`NativeActivity` 只覆盖了 Activity 一种，`Service` / `BroadcastReceiver` / `ContentProvider` 没有任何 native 等价物。所以「完全不用 Java」在这个应用形态下不成立。能成立的是「**Java 里没有一行业务逻辑**」，这条本项目守住了。

### JNI 边界

Rust 导出（`crates/magpie-android/src/lib.rs`）：

```rust
JNI_OnLoad                                      // 保存 JavaVM

Java_io_magpie_rs_MainActivity_nativeStart      // (filesDir, versionName, versionCode, sdk)
Java_io_magpie_rs_MainActivity_nativeAttachWebView   // (WebView)
Java_io_magpie_rs_MainActivity_nativeCheckUpdate     // (silent)
Java_io_magpie_rs_MainActivity_nativeUpdateChoice    // (1 现在 / 2 稍后 / 3 跳过)
Java_io_magpie_rs_MainActivity_nativeStatus          // → JSON 字符串
Java_io_magpie_rs_MainActivity_nativeStop

Java_io_magpie_rs_MagpieService_nativeEnsureRunning  // (filesDir)
Java_io_magpie_rs_MagpieService_nativeStop
Java_io_magpie_rs_MagpieService_nativeRunning        // → boolean
```

Rust 调 Java（`webview.rs` / `lib.rs`）：

```java
WebView.loadUrl(String)
WebView.evaluateJavascript(String, ValueCallback)
Toast.makeText(Context, CharSequence, int).show()
io.magpie.rs.ShellUi.showUpdate(Context, String version, String size, String notes)
io.magpie.rs.ShellUi.updateProgress(Context, int percent)
io.magpie.rs.ApkInstaller.install(Context, String apkPath)
```

每个 JNI 入口都包了 `catch_unwind`：`panic = "abort"` 会把一个请求里的 bug 变成整个网关死掉，这个取舍在 `Cargo.toml` 里写明了。

### 线程模型

```
UI 线程 ──nativeStart──► 起一个 std::thread
                              └─ rt::start(): 建 tokio multi-thread (2 worker)
                                   ├─ 绑 3425 / 3430
                                   └─ axum serve + 页面 serve
                          等待页面端口可连（最多 15 s）→ webview.load_page()

Service 线程 ──nativeEnsureRunning──► 已经在跑就返回（幂等）

任一 JNI 回调 ──jvm::with_env()──► attach_current_thread（首次 attach，之后复用）
```

`rt::stop()` 通过 `watch` channel 通知两个 listener 走 graceful shutdown，然后 `shutdown_timeout(1s)` 强制回收。之所以不用 `Notify`：`notify_waiters()` 只唤醒已注册的 waiter，`watch` 的 `changed()` 不会漏掉"信号先发、等待后到"这种情况。

## 3. 原依赖在 Rust 生态的对应

| 原（Go） | Rust | 说明 |
|---|---|---|
| `net/http` + 自建 mux | `axum` 0.8 | 路由 + 提取器 |
| `net/http/httputil` 反向代理 | `reqwest` 0.13 | 需要流式，所以用 `bytes_stream()` 而不是 `text()` |
| 手写 SSE 解析 | `protocol::SseParser`（40 行） | 上游三个厂商的格式都只有 `event:` / `data:` 两种行，不值得引 `eventsource-stream` |
| `tidwall/gjson` | `serde_json` | 直接解析成 `Value`，比 gjson 的路径查询更直观 |
| `go-toml` / `yaml.v3` | 不需要 | 配置统一 JSON |
| `modernc.org/sqlite` | 不需要 | 只用于读别人的库 |
| `moejs`（JS 中间件） | **无对应** | 不做 |
| `wails/v3` | **无对应** | `nogui` 已排除 |
| `bubbletea` | **无对应** | 不做 TUI |
| `internal/appdir` | `config.rs` 里的路径计算 | 保持 `$XDG_CONFIG_HOME/magpie` |
| 系统 keychain | **无对应** | 只支持配置文件里的 API key |
| Go 的 `embed.FS` | `include_str!` | 页面只有一个文件 |

选 `axum` 而不是 `hyper` 手写：`hyper` 的服务端 API 需要自己接 `Tower`、自己处理 `Accept`、自己写提取器，对 8 个端点来说是负收益。

选 `reqwest` 而不是 `hyper-util` 手写 client：连接池、重定向、代理、TLS 都要自己接一遍，而且它的 `bytes_stream()` 正好是流式转发需要的形状。

## 4. 无法直接迁移的部分及处理

| 部分 | 处理 |
|---|---|
| JS 插件系统（Bun 子进程 + npm 包） | 不做。README 说明了原因（Bun 的 JIT 在 Android seccomp 下 `bad system call`，上游自己得关 JIT；体积 176 MB） |
| JS 网关中间件（`onRequest`/`onEvent`/`onResponse`） | 不做。Rust 侧没有可用的轻量 JS 引擎 |
| 订阅账号登录（Claude/Codex/Copilot…） | 不做。统一走 API key |
| 会话抓取、Library 注入、agent 配置改写 | 不做。手机上这些 agent 不在同一设备 |
| 余额/配额/并发限流/MCP 桥/绘图/搜索/转写 | 不做 |
| Wails 桌面窗口、托盘、TUI | 不做 |
| 备份同步（WebDAV/S3/GitHub） | 不做 |

**处理原则**：不假装实现了。README 里有一张"没有实现（明确标注）"的表，逐项写了原因；`/v1/*` 上不存在的端点会返回 404 而不是静默 200。

## 5. 三个必须踩对的平台细节

### 16 KB 页

Android 15+ 的设备可能用 16 KB 内核页，加载器会拒绝 PT_LOAD 只按 4 KB 对齐的 `.so`。两处都要管：

1. **ELF 段对齐**：`.cargo/config.toml` 里给两个 target 加 `-C link-arg=-Wl,-z,max-page-size=16384`。验证方式是 `llvm-readelf -l` 看 `LOAD ... Align 0x4000`。
2. **zip 内偏移对齐**：`zipalign -f -P 16 4`。注意 `-P`（页大小 KB）和 `-p`（4 KB，等价于 `-P 4`）**互斥**，同时传会让 zipalign 直接失败——这一点在 build-tools 的 usage 里写着，但不看就会踩。

### 原生库的存放路径

`System.loadLibrary("magpie")` 只认 APK 里的 `lib/<abi>/libmagpie.so`。打包脚本是"先生成资源 APK，再把 dex 和 .so 塞进去"，所以塞的时候 entry 名必须自己拼上 `lib/` 前缀——漏了不会报错，装上去才 `UnsatisfiedLinkError`。

`.so` 用 `-0`（不压缩）存储：`extractNativeLibs=true` 时系统会把它拷到 `nativeLibraryDir`，未压缩拷贝更快；`extractNativeLibs=false` 时则直接 mmap，未压缩是硬要求。

### 明文 HTTP 只给 loopback

`res/xml/network_security_config.xml` 里只给 `127.0.0.1` / `localhost` 开 `cleartextTrafficPermitted`，其余保持禁止。注意这个策略**只管 WebView 和 Java 的 HTTP 栈**，Rust 的 reqwest/rustls 走自己的根证书，不受它约束——所以 provider 用什么协议、走不走 https，是配置里的 `base_url` 决定的，不是这个文件。

## 6. 不依赖 Gradle 的打包

AGP 做的是这七件事，本项目把它们写出来：

```
cargo build --target <abi>     → libmagpie.so
aapt2 compile --dir res        → resources.zip
aapt2 link -I android.jar ...  → base.apk（资源 + manifest）
javac -classpath android.jar   → classes/
d8 --lib android.jar           → classes.dex
zip                            → base.apk += classes.dex + lib/<abi>/*.so
zipalign -P 16 4 + apksigner   → app-release.apk
```

为什么不用 Gradle：

- **不需要联网**。AGP 首次构建要拉几百 MB 的 Maven 依赖，一个"clone 下来就能打 APK"的项目不该有这一步。
- **不需要 AGP 版本与 Gradle 版本对齐**。这两个的兼容矩阵本身就是一类日常故障源。
- **可审计**。7 步、每步一条命令，出问题看一眼就知道卡在哪。
- 代价：没有 `BuildConfig`、没有变体（flavor）、资源合并是最小实现。对这个应用（8 个 Java 文件、6 个资源）完全够。

CI 里跑的是同一份逻辑的 bash 版本（`scripts/build-apk.sh`），矩阵编两个 ABI，最后汇总签名。

## 7. 应用内更新

数据源就是本仓库的 GitHub Release（见 `.github/workflows/release.yml`，打 `v*` tag 自动挂 APK）。实现见 `crates/magpie-android/src/update.rs`，行为见 README 的「应用内更新」。

三个刻意的选择：

1. **`PackageInstaller` 而不是 `ACTION_VIEW` + FileProvider**：平台给应用自更新的正规路径，不需要把 APK 暴露成 `content://`，也不需要 FileProvider 这个 androidx 依赖。
2. **先写 `.part` 再 rename**：截断的 APK 装起来报的错和"文件损坏"毫无关系。
3. **所有失败分支都有确定的用户可见结果**：404、断网、版本号解析不出来、asset 里没有 `.apk`，各有各的说法；静默检查时全部吞掉（只有用户主动点才 toast）。
