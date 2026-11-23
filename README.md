# magpie-rs

**Every agent's model. One place.** — 一个用 Rust 重写的 [magpie](https://github.com/yetone/magpie) 网关，以及它的 Android 壳。

装到手机上，网关就在应用进程里跑起来（默认 `127.0.0.1:3425`），Claude Code、Codex、Gemini CLI 这些跑在 Termux 里的 agent 把 base URL 指过来，就能用你配好的任意供应商的模型。magpie 自己的配置网页也一起带着（默认 `127.0.0.1:3430`）。

> 本项目是 [gllhack/magpie-android](https://github.com/gllhack/magpie-android) 的 Rust 重构版。原项目是「Go 网关二进制 + Java 壳」，本项目把**网关和壳都改成 Rust**，产出的 APK 从 268 MB 降到 **6.4 MB**——没有随包的 Bun、没有 Go 运行时、没有两个架构的网关二进制。

---

## 目录

- [这是什么](#这是什么)
- [功能](#功能)
- [架构](#架构)
- [两个关键设计决定](#两个关键设计决定)
- [构建](#构建)
- [安装与使用](#安装与使用)
- [应用内更新](#应用内更新)
- [依赖版本](#依赖版本)
- [与原项目的差异](#与原项目的差异)
- [许可证](#许可证)
- [致谢](#致谢)

---

## 这是什么

magpie 解决的问题：手机上的编码 agent（Claude Code、Codex、Gemini CLI）只认一种 API 形状，而你手上有 DeepSeek、Kimi、通义、Claude、Gemini、本地 Ollama……各不相同。

magpie 在中间做一件事：**把任意供应商的模型，翻译成 agent 认的那个形状**。

```
Claude Code  ──/v1/messages──┐
Codex        ──/v1/responses─┤
Gemini CLI   ──/v1beta───────┴──►  magpie 网关  ──►  供应商 A (openai 协议)
                                    :3425            供应商 B (anthropic 协议)
                                                     供应商 C (gemini 协议)
```

magpie-rs 是这个网关的 Rust 实现，外加一个 Android 前端：WebView 显示配置页，前台服务保证进程不被冻结，开机自启，以及在应用内检查/下载/安装新版本。

## 功能

网关侧：

| | 功能 | 说明 |
|---|---|---|
| ✅ | OpenAI 入口 | `POST /v1/chat/completions`、`GET /v1/models`，含 SSE 流式 |
| ✅ | Anthropic 入口 | `POST /v1/messages`、`/v1/messages/count_tokens`，含事件流 |
| ✅ | 三种上游协议 | `openai` / `anthropic` / `gemini`，任意 base URL |
| ✅ | 协议互译 | OpenAI ↔ Anthropic 双向，Gemini 只做入口（见[差异](#与原项目的差异)） |
| ✅ | 工具调用 | `tools` / `tool_use` / `tool_result` / `functionCall` 在各协议间转换，流式也一样 |
| ✅ | 模型路由 | 裸模型名、`provider/model`、按 `priority` 排序、`order` / `rotate` / `failover` 三种模式 |
| ✅ | 网关密钥 | `sk-magpie-key-…`、等长比较、LAN 开启时强制 |
| ✅ | 来源 IP 白名单 | 支持 `192.168.1.*` 前缀通配 |
| ✅ | 用量账本 | `usage.jsonl`，每请求一行，网页里可看聚合 |
| ✅ | 配置网页 | 单文件 HTML，无构建步骤；provider 增删改、路由设置、日志查看、密钥轮换 |
| ✅ | 结构化错误 | `model_not_found` / `authentication_error` / `upstream_error`，按入口协议返回对应形状 |

Android 壳：

| | 功能 | 说明 |
|---|---|---|
| ✅ | 前台服务 | 常驻通知（打开 / 停止），进程不被冻结 |
| ✅ | WebView 承载 | 页面只绑 loopback，带 key；外链和 `window.open` 交给系统浏览器 |
| ✅ | 局域网开关 | 绑 `0.0.0.0`，并强制要求网关密钥 |
| ✅ | 开机自启 | `BOOT_COMPLETED` + `MY_PACKAGE_REPLACED` |
| ✅ | 应用内更新 | 查 GitHub Release → 比对版本 → 下载 → `PackageInstaller` 安装 |
| ✅ | 端口可改 | 网关端口和页面端口都能改，改完立刻生效 |
| ✅ | 崩溃自愈 | 服务 `START_STICKY`，进程被杀后系统重启服务，网关随之恢复 |

## 架构

```
┌──────────────────────────── Android 应用进程 ────────────────────────────┐
│                                                                         │
│  MainActivity (Java, 60 行)          MagpieService (Java, 90 行)        │
│    └─ 造一个 WebView                  └─ 前台通知 + START_STICKY        │
│         │                                  │                            │
│         │ JNI                              │ JNI                        │
│         ▼                                  ▼                            │
│  ┌──────────────────── libmagpie.so (Rust) ────────────────────┐        │
│  │  magpie-android crate                                       │        │
│  │    lib.rs      JNI 导出，catch_unwind 包每个入口            │        │
│  │    rt.rs       一个 tokio runtime，网关生命周期             │        │
│  │    webview.rs  loadUrl / evaluateJavascript / toast         │        │
│  │    update.rs   查 Release、比版本、下载 APK                 │        │
│  │    shell.rs    shell.json（端口、LAN、自启、跳过的版本）    │        │
│  │         │                                                   │        │
│  │         ▼                                                   │        │
│  │  magpie-core crate（纯 Rust，也能在桌面跑）                 │        │
│  │    gateway.rs   axum 路由、鉴权、上游选择、流式转发         │        │
│  │    protocol/    openai.rs / anthropic.rs / gemini.rs        │        │
│  │    ir.rs        中间表示（两种方向都要转的字段才进来）      │        │
│  │    route.rs     模型 → 供应商                              │        │
│  │    config.rs    settings.json / providers.json              │        │
│  │    usage.rs     usage.jsonl                                 │        │
│  │    web/         配置页（一个 HTML，include_str! 内嵌）      │        │
│  └─────────────────────────────────────────────────────────────┘        │
│                          │                                              │
│              127.0.0.1:3425 (网关)  127.0.0.1:3430 (页面)              │
└─────────────────────────────────────────────────────────────────────────┘
```

仓库结构：

| 路径 | 内容 |
|---|---|
| `crates/magpie-core/` | 网关本体。不依赖 Android，`cargo run -p magpie-core -- serve` 就能在桌面上起 |
| `crates/magpie-android/` | Android 壳的 Rust 部分，编译成 `libmagpie.so` |
| `android/app/src/main/java/io/magpie/rs/` | 8 个 Java 文件，约 400 行，全是平台胶水 |
| `android/app/src/main/res/` | 图标、字符串、主题、网络安全策略 |
| `scripts/build-apk.ps1` / `.sh` | 不依赖 Gradle 的打包脚本（Windows / Linux） |
| `.github/workflows/release.yml` | 打 tag 自动出 APK 并挂 Release |

**Java 侧只有 8 个文件**，每个的存在理由都是「Android 要求它必须是 Java 类」：

| 文件 | 行数 | 为什么不能省 |
|---|---|---|
| `MainActivity` | ~140 | Activity 必须由系统实例化，`NativeActivity` 又拿不到 WebView |
| `MagpieWebClient` | ~60 | `WebViewClient` 是抽象类，不能用动态代理 |
| `MagpieChromeClient` | ~50 | `onCreateWindow` 同上，处理 `window.open` |
| `MagpieService` | ~130 | 前台服务没有 native 等价物 |
| `BootReceiver` | ~25 | 广播接收器同上 |
| `ShellUi` | ~90 | `AlertDialog` / `ProgressBar` 是平台控件 |
| `ApkInstaller` | ~70 | `PackageInstaller` 是平台 API |
| `InstallReceiver` | ~35 | 安装结果回执 |
| `ShellPrefs` | ~75 | 开机时无需加载 native 库就能读配置 |

所有**判断**都在 Rust：哪个端口、页面去哪、要不要更新、更新到什么版本、请求怎么翻译、失败怎么办。Java 只做「造控件 / 收广播 / 转发回调」。

## 两个关键设计决定

### 1. 网关跑在应用自己的进程里，不再 exec 一个子进程

原项目把 Go 编成 `libmagpie.so`，因为 APK 只能通过 `jniLibs` 携带可执行文件，而 Android 10+ 只允许从 `nativeLibraryDir` 执行。于是它有了：`nativeLibraryDir` 里躺着一个 98 MB 的"库"、一个进程内 `ProcessBuilder`、一套看门狗重启退避、以及 `LD_PRELOAD` 清理。

Rust 版是 `cdylib`，本来就该被 `System.loadLibrary` 加载。网关在同一个进程的 tokio runtime 里跑：

- 少一次 fork/exec，启动快 ~300 ms
- 日志直接进同一个 logger，不再靠 stdout 管道转发
- 崩溃语义变了：网关 panic 不再"子进程退出"，而是由 `catch_unwind` 在每个 JNI 入口兜住；真挂了就是应用进程挂了，服务 `START_STICKY` 会把它拉起来

代价：网关和 UI 共享一个进程的资源和 OOM 风险。以这个网关的内存占用（空闲 ~8 MB）来说，值得。

### 2. TLS 后端选 `ring`，不选 `aws-lc`

reqwest 0.13 默认把 rustls 的 crypto provider 换成 aws-lc，而 **aws-lc-sys 需要 CMake 和 NASM** 才能构建。对一个「clone 下来就能打 APK」的项目来说，这两个都是不该有的前置条件。

所以 `Cargo.toml` 里关掉默认 provider，显式选 `ring`：

```toml
reqwest = { version = "0.13", default-features = false, features = [
    "json", "stream", "charset", "http2", "rustls-no-provider",
] }
rustls = { version = "0.23", default-features = false, features = ["ring", "std", "tls12", "logging"] }
```

`ring` 只需要一个 C 编译器（Android 上是 NDK 的 clang，Windows 上是 MSVC 的 cl），`install_default()` 在 `Gateway::new` 里调一次。

## 构建

### 需要什么

| | 版本 | 备注 |
|---|---|---|
| Rust | stable（本文用 1.100.0） | `rustup target add aarch64-linux-android x86_64-linux-android` |
| Android SDK | platform 36 + build-tools 37.0.0 | 只要 `android.jar`、`aapt2`、`d8`、`zipalign`、`apksigner` |
| Android NDK | r30（`30.0.16248370`） | 交叉编译 `libmagpie.so` |
| JDK | 21 | `javac`、`jar`、`keytool` |

**不需要 Gradle，不需要 AGP，不需要联网构建。** 打包流程就是 7 步 shell，`scripts/build-apk.ps1` 和 `scripts/build-apk.sh` 逐行可读。

### 一条命令

Windows：

```powershell
# 一次性：装工具链到 D:\magpie-rs\toolchain，不碰系统 PATH 和注册表
pwsh -File D:\magpie-rs\toolchain\setup-toolchain.ps1

# 每次构建
cd <repo>
. D:\magpie-rs\toolchain\env.ps1
.\scripts\build-apk.ps1 -Release
```

Linux / macOS：

```bash
export ANDROID_HOME=$HOME/android-sdk
export NDK_HOME=$ANDROID_HOME/ndk/30.0.16248370
./scripts/build-apk.sh --release
```

产物：

```
android/app/build/outputs/apk/app-release.apk
```

### 参数

| 参数（ps1 / sh） | 作用 |
|---|---|
| `-Release` / `--release` | release 构建（默认 debug） |
| `-Abis arm64-v8a,x86_64` / `--abis arm64-v8a,x86_64` | 只编指定 ABI |
| `-SkipRust` / `--skip-rust` | 复用已编好的 `.so`，只重打包 |
| `-VersionName 1.2.0 -VersionCode 3` / `--version-name 1.2.0 --version-code 3` | 版本号写进 manifest |
| `-SigningKey path` / `--signing-key path` | 用自有 keystore 而不是自动生成的 debug key |

### 只跑网关（桌面，不用手机）

```bash
cargo run -p magpie-core -- serve --config ./dev-config
# 另一个终端
curl http://127.0.0.1:3425/health
curl -s http://127.0.0.1:3425/v1/models
```

`cargo test` 会跑协议翻译的往返测试。

### CI

`.github/workflows/release.yml`：

- push / PR：`cargo fmt --check` + `clippy -D warnings`
- 矩阵编 arm64-v8a 和 x86_64，各自上传 `libmagpie.so`
- 汇总任务装 APK、可选地用 secrets 里的正式签名重签
- **打 `v*` tag 时自动挂到 GitHub Release** —— 应用内更新的数据源就是它

正式签名需要三个 secret：

| Secret | 内容 |
|---|---|
| `ANDROID_KEYSTORE_BASE64` | `base64 -w0 release.keystore` |
| `ANDROID_KEYSTORE_PASSWORD` | store 密码 |
| `ANDROID_KEY_ALIAS` / `ANDROID_KEY_PASSWORD` | 别名与密码 |

没有这三个 secret 时，CI 出的是 debug 签名的包（能装、能更新，但不能上架）。

## 安装与使用

```bash
adb install -r android/app/build/outputs/apk/app-release.apk
```

1. 打开 magpie，它会自己起网关，界面就是配置页。
2. 在 **Providers** 里加一个供应商：`id`、`protocol`、`base URL`、`API key`、模型列表（留空表示该供应商接受任意模型名）。
3. **Settings** 里可以改路由模式、要求网关密钥、限制来源 IP。
4. **About** 里能看到监听地址和日志路径。

然后把这个填给 agent：

```
base URL : http://127.0.0.1:3425/v1
api key  : 你在 Settings → Keys 里生成的 sk-magpie-key-…
model    : deepseek-chat          或显式写  deepseek/deepseek-chat
```

端口：

| 端口 | 用途 | 默认 |
|---|---|---|
| 3425 | 网关，agent 指这里 | `127.0.0.1:3425`，开 LAN 后 `0.0.0.0:3425` |
| 3430 | 配置页，**始终只绑 loopback** | `127.0.0.1:3430` |

页面密钥在首次启动时随机生成（32 位），存在 `settings.json` 里，WebView 每次都直接进得去。用菜单里的地址能在电脑上打开同一个页面。

## 应用内更新

流程（`crates/magpie-android/src/update.rs` + `lib.rs`）：

```
启动 5 秒后（静默）          用户点「立即更新」
      │                            │
      ▼                            ▼
GET /repos/{REPO}/releases/latest ──► 解析 tag_name + 第一个 .apk asset
      │
      ├─ 404 ────────────► "这个仓库还没有 Release"
      ├─ 网络失败 ───────► "连不上 GitHub"（静默模式什么都不说）
      ├─ tag 解析失败 ───► 当作「没有更新」，不让比较逻辑抛
      ├─ 不比当前版本新 ─► 静默 / toast "已是最新"
      └─ 有新版本 ───────► AlertDialog：立即更新 / 稍后 / 跳过此版本
                                │
                                ▼
                        下载到 files/update/magpie-<tag>.apk
                        （先写 .part，完成再 rename；进度条 0–100）
                                │
                                ▼
                        PackageInstaller 会话 + InstallReceiver 回执
```

几个刻意的选择：

- **不用 FileProvider / `ACTION_VIEW`**。`PackageInstaller` 是平台给应用自更新的正规路径，不需要把文件暴露成 `content://`，也不需要引导用户去开「未知来源」。
- **`.part` 再 rename**。半途中断留下一个截断的 APK，安装时报的错和"文件损坏"完全无关，很难查。
- **跳过的版本存在 `shell.json`**，不是内存里，重启也记得。
- **静默检查只在启动时跑一次**，之后靠用户主动点。手机上每几小时打一次 GitHub 既不礼貌也费电。
- 版本比较是 `v1.2.3` / `1.2.3` / `1.2.3-rc1` 都能比的数字段比较，预发布后缀被忽略，非数字段按 0 处理。

仓库地址编译期注入，fork 出去改一个环境变量就行：

```bash
MAGPIE_UPDATE_REPO=yourname/magpie-rs cargo build -p magpie-android --release --target aarch64-linux-android
```

（不设时默认 `1527183723/magpie-rs`，见 `update.rs` 的 `REPO`。）

## 依赖版本

| 依赖 | 版本 | 为什么是这个 |
|---|---|---|
| Rust | 1.100.0 (2026-11-12) | 当下 stable |
| edition | 2024 | 需要 `resolver = "3"` |
| axum | 0.8.9 | 网关的 HTTP 层 |
| tokio | 1.54.1 | 运行时（`rt-multi-thread`、`signal`） |
| reqwest | 0.13.5 | 上游调用；关了默认 TLS 后端，见[设计决定 2](#2-tls-后端选-ring不选-aws-lc) |
| rustls | 0.23.46 | `ring` provider |
| serde / serde_json | 1.0.229 / 1.0.154 | 配置与协议体 |
| futures-util | 0.3.34 | SSE 流 |
| jni | **0.21.1** | 见下方说明 |
| Android NDK | r30 `30.0.16248370` | sysroot 支持到 API 37 |
| Android platform | API 36（Android 16） | `compileSdk` = `targetSdk` = `minSdk` = 36 |
| build-tools | 37.0.0 | `aapt2` / `d8` / `zipalign` / `apksigner` |
| JDK | Temurin 21 | `javac --source 17 --target 17` |

**关于 `jni` 版本**：crates.io 上最新是 0.22.4，但 0.22 把 native 入口类型从 `JNIEnv` 换成了 `EnvUnowned` + 回调式 attach，`GlobalRef` 也加了生命周期参数——这是一次尚未被 Android 生态消化的重写。本项目用 0.21.1，它的 API 稳定、文档齐全，也是绝大多数在维护的 Android Rust 项目用的版本。升级路径写在这里，不是忘了升。

## 与原项目的差异

诚实清单。原项目是 [gllhack/magpie-android](https://github.com/gllhack/magpie-android)，它打包的是 [yetone/magpie](https://github.com/yetone/magpie) 上游 main 分支的完整快照——**约 30 万行 Go**（`internal/` 非测试代码 10.5 MB），其中 `internal/gateway` 一项就是 21 万行。

想在合理工期内逐行重写这 30 万行不现实，所以 magpie-rs 实现的是**「agent 指向 3425 端口时实际会用到的那部分」**，其余部分明确列在下面。

### 已经对齐

- 端口契约：网关 3425、页面 3430，`MAGPIE_ADDR` / `MAGPIE_WEB_KEY` 环境变量
- 配置文件布局：`$XDG_CONFIG_HOME/magpie/{settings,providers}.json`（Android 上是 `files/config/`）
- 用量账本：同名 `usage.jsonl`，字段形状接近
- 页面鉴权：`?k=` 换 cookie，cookie 名 `magpie_web_<port>`，400 天
- 前台服务 + 常驻通知 + 停止动作
- `window.open` 交给系统浏览器
- 局域网开关强制要求网关密钥

### 实现了，但覆盖面窄

| 项 | 上游 | magpie-rs |
|---|---|---|
| 协议互译 | OpenAI / Anthropic / Gemini / Responses / images / embeddings / rerank / audio / videos，7 个方向全通 | **OpenAI ↔ Anthropic 双向**；Gemini 只能作为上游出口（入口不支持），不含 Responses API |
| 路由策略 | `smart` / `order` / `rotate` / `usage` / `pace`，可嵌套分组、会话粘性、intent routing | `order` / `rotate` / `failover` 三种，按 `priority` 排序 |
| 供应商 | 50+ 预设 + 订阅账号（Claude/Codex/Copilot/Grok/Cursor…，含 keychain 读取、OAuth、warm-up） | **任意 OpenAI/Anthropic/Gemini 兼容端点 + API key**。订阅账号登录不做 |
| 配置页 | Svelte 应用（5 MB），`/api/*` 全套 | 单文件 HTML，providers/settings/log/usage 四块 |
| 用量 | `usage.jsonl` + CSV 导出 + 价格表 + 热图 | `usage.jsonl` + 按供应商聚合 |

### 没有实现（明确标注）

| 项 | 原因 |
|---|---|
| **JS 插件系统（Bun）** | 上游靠一个 Bun 子进程跑 `host.js`（108 KB）承载 npm 包形式的 OpenCode/pi 插件。Android 上要随包带两个架构的 Bun（176 MB），而且 Bun 的 JIT 在 Android 的 seccomp 下会 `bad system call`，上游得靠 `BUN_JSC_useJIT=0` 绕。Rust 版不带 JS 运行时，插件页不存在 |
| **JS 网关中间件（moejs）** | 上游用纯 Go 的 JS 引擎在进程内编译执行 `onRequest`/`onEvent`/`onResponse`。Rust 侧没有等价引擎（无 `boa`/`rquickjs` 依赖），不做 |
| **会话抓取（sessions）** | 上游只读打开 OpenCode 的 SQLite 库，统计各 agent 的会话。Android 上没有这些库可读 |
| **Library 注入 / agent 配置改写** | 上游会把 instructions、MCP servers、skills 写进各 agent 的配置文件（原子写、保留注释）。手机上这些 agent 不在同一设备，或者用户自己管 |
| **余额查询、配额、并发限流、MCP 桥、绘图/搜索/转写端点** | 未实现 |
| **TUI、托盘、Wails 桌面窗口** | 上游 `nogui` 构建标签已经把这些排除掉了；magpie-rs 干脆没有 |
| **备份同步（WebDAV / S3 / GitHub）** | 未实现 |
| **`package.json` 形式的插件页** | 与 JS 插件系统同源，不做 |

### 行为上的差异

- **启动方式**：原项目 exec 一个子进程并看门狗重启（退避 1/2/5/15/30 s）；magpie-rs 在进程内跑，进程没了服务会重启进程。所以「网关重启」这件事在日志里的样子不同。
- **日志**：原项目把子进程 stdout 管道写进 `magpie.log`；magpie-rs 直接写同一个文件，另有一个 2000 行的内存环给网页读。
- **`minSdk`**：原项目 36（Android 16）。本项目和它一致——不是为了追新，而是想少写兼容分支；想降下来改 `build-apk.ps1` 的 `--min-sdk-version` 和 manifest 即可，壳代码没用到 36 独有的 API。

## 许可证

[MIT](LICENSE)。

上游 [yetone/magpie](https://github.com/yetone/magpie) 和 [gllhack/magpie-android](https://github.com/gllhack/magpie-android) 都是 MIT，本项目沿用同一许可证，衍生部分的条款见 `LICENSE` 末尾。

## 致谢

- [yetone/magpie](https://github.com/yetone/magpie) — 全部设计、端口约定、配置布局和「一个地方接所有模型」的想法都来自这里
- [gllhack/magpie-android](https://github.com/gllhack/magpie-android) — Android 打包路径、`jniLibs` 携带可执行文件、`window.open` 处理、局域网/开机自启这些「手机上才知道的坑」，都是它先踩出来的
- [axum](https://github.com/tokio-rs/axum) / [tokio](https://github.com/tokio-rs/tokio) / [reqwest](https://github.com/seanmonstar/reqwest) / [rustls](https://github.com/rustls/rustls) / [jni-rs](https://github.com/jni-rs/jni-rs)

---

## 状态

真机实测（Xiaomi，Android 17 / API 37）：

| 版本 | 结果 |
|---|---|
| v1.0.0 / v1.0.1 | **启动即崩**：`RuntimeException: A WebView method was called on thread 'Thread-4'` → `WebView.java:2669 → loadUrl(WebView.java:747)` |
| v1.0.2 | 修掉该崩溃，待验证 |

已修：

- **WebView 跨线程调用**（致命）。Rust 的 `std::thread::spawn` 线程直接调 `WebView.loadUrl`，`checkThread` 抛异常、后台线程无人捕获、进程 abort。现在所有 WebView 操作都经 `io.magpie.rs.WebOps` 走 `Activity.runOnUiThread`；字节码已验证 `loadUrl`/`loadDataWithBaseURL` 的**唯一**调用点就在这个转发器的 lambda 里。
- **重复启动页面加载器**。`nativeStart` 与 `nativeAttachWebView` 各 spawn 了一次，所以每次崩溃都是成对的两条 FATAL。现在用 `AtomicBool` 保证只启动一次。
- **端口冲突**。默认 3425/3430 与本项目的前身 `com.magpie.android` 相同，两个都装时后者会先占住端口，导致 `Address already in use (os error 98)`。现在启动时探测 3425–3432 / 3430–3437，取第一个可绑的端口并写回 `shell.json`，通知栏与页面显示的都是实际端口。
- **日志时间戳恒为 `[0.000]`**。v1.0.1 把毫秒硬编码成了 0，200 ms 的启动和 20 s 的看起来一样。现在是真实毫秒。
- **`ApkInstaller` 在非 UI 线程弹 Toast**，会抛 `Can't create handler inside thread that has not called Looper.prepare()`。改走 `ShellUi.toast`。

已验证（构建与网关侧）：

- 网关在 Windows 上 `cargo run -p magpie-core -- serve` 跑通完整链路：模型目录、OpenAI 入口、Anthropic→OpenAI 协议翻译（含 usage / finish_reason）、显式 `provider/model` 路由、未知模型 404、`usage.jsonl` 账本
- `cargo test -p magpie-core` 9 个往返测试全过；`cargo fmt --check` 与 `cargo clippy --workspace --all-targets -- -D warnings` 无输出
- CI（Ubuntu）从零构建通过，打 tag 自动挂 Release；本地与 CI 的包证书指纹一致，可互相覆盖升级
- 应用内更新的数据源：`releases/latest` 返回 `v1.0.0` + `.apk` asset，`update.rs` 的选取规则与版本比较都用真实响应验证过

待验证：

- v1.0.2 能否在真机上正常启动
- `PackageInstaller` 更新流程与更新对话框尚未在设备上点过

设计取舍见 [docs/PLAN.md](docs/PLAN.md)，`jni` 版本与升级路径见 [docs/MIGRATING.md](docs/MIGRATING.md)。