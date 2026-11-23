# 阶段一：原仓库分析

对象：[gllhack/magpie-android](https://github.com/gllhack/magpie-android)（master，2026-11-23 抓取）
上游：[yetone/magpie](https://github.com/yetone/magpie)（main，2026-10-16 快照）

## 结论先行

原项目**不是一个应用，而是两个东西捆在一起**：

1. `upstream/` —— magpie 上游 Go 源码的原样快照，**5848 个文件**，`internal/` 下 3301 个 `.go` 文件、24.8 MB 源码（非测试 10.5 MB ≈ **30 万行**）。这部分完全没有改动。
2. `android/` —— 一个很小的 Java 壳，**5 个类、约 59 KB**，负责把 Go 二进制拉起来、给个 WebView、加个通知栏。

所以「用 Rust 重构该项目」的真实工作量分布是：**壳 1%，网关 99%**。这个比例决定了重构方案必须先划边界再动手，否则就是把 magpie 整个重写一遍。

## 1. 功能清单

### 壳（原项目自己写的部分）

| 类 | 行数量级 | 职责 |
|---|---|---|
| `MagpieGateway` | 10.5 KB | 从 `nativeLibraryDir` exec `libmagpie.so serve --web=127.0.0.1:3430`；设 `HOME`/`XDG_*`/`TMPDIR`/`MAGPIE_ADDR`/`MAGPIE_WEB_KEY`/`MAGPIE_BUN`/`BUN_JSC_useJIT=0`；清 `LD_PRELOAD`；stdout 管道写日志；崩溃看门狗（1/2/5/15/30 s 退避）；日志 2 MB 轮转 |
| `MagpiePrefs` | 3 KB | SharedPreferences：网关端口 3425、页面端口 3430、LAN 开关、开机自启、页面 key（32 位随机） |
| `MagpieService` | 3.7 KB | 前台服务 + 常驻通知（打开 / 停止）+ `START_STICKY` |
| `BootReceiver` | 0.6 KB | `BOOT_COMPLETED` / `MY_PACKAGE_REPLACED` 时按开关启动 |
| `MainActivity` | 31 KB | 纯代码构建的 UI：Toolbar + WebView + 底部抽屉菜单 + 设置对话框 + 日志面板 + 复制地址 / 浏览器打开 / 忽略电池优化；`onCreateWindow` 把 `window.open` 交给系统浏览器 |

### 网关（上游 magpie）

按代码量排：

| 模块 | 规模 | 能力 |
|---|---|---|
| `internal/gateway` | 21 万行（含测试） | HTTP 端点、协议互译、路由、失败转移、绘图/搜索/转写 |
| `internal/provider` | 7.7 万行 | 供应商预设、密钥、订阅账号、模型目录、余额 |
| `internal/gui` | 107 文件 + 5.1 MB 前端 | Wails 窗口、托盘、`magpie web`、`/api/*` |
| `internal/routes` | 52 KB 单文件 | `smart`/`order`/`rotate`/`usage`/`pace` 策略 |
| `internal/sessions` `agent` `library` `plugin` `usage` | 180/115/51/62/28 文件 | 会话抓取、agent 配置改写、skills 注入、JS 插件、用量 |

## 2. 模块划分

```
magpie/
├── main.go                 # 504 B，只调 cli.Main(version) + 注册内置插件
├── internal/
│   ├── gateway/            # 网关与协议翻译（210 文件）
│   ├── provider/           # 供应商与账号（227 文件）
│   ├── gui/                # 桌面壳 + 网页
│   ├── cli/                # 命令集，含 nogui 构建标签分支
│   ├── plugin/ pluginrt/   # Bun 子进程插件宿主
│   ├── jsmod/ middleware/  # moejs 进程内 JS 中间件
│   ├── sessions/ agent/ library/ usage/
│   └── appdir/             # 唯一的路径来源
├── plugins/                # 15 个编译期内置插件（41 KB）
└── site/                   # 60 MB 静态官网（不参与运行时）
```

## 3. 依赖（`upstream/go.mod`）

`go 1.26.8`，18 个直接依赖，关键几个：

| 依赖 | 用途 | Rust 重构时的处置 |
|---|---|---|
| `wailsapp/wails/v3` | 桌面窗口（WebView2/WebKitGTK），**cgo** | `nogui` 已经排除，Rust 版不要 |
| `Calcium-Ion/moejs` | 纯 Go 的 JS 引擎 | 无等价物，不做 |
| `charmbracelet/bubbletea` | TUI | 不做 |
| `modernc.org/sqlite` | 纯 Go SQLite | 只用于**只读**打开别人的会话库，不做 |
| `tidwall/gjson` / `jsonc` | JSON 快速取值 | `serde_json` |
| `pelletier/go-toml/v2` / `yaml.v3` | 解析外部工具配置 | 不需要（我们的配置是 JSON） |
| `klauspost/compress` | gzip 中间件 | 不做（loopback 不需要压缩） |

`CGO_ENABLED=0 -tags nogui` 才能交叉编译出 Android 二进制 —— 这正是原项目能把它塞进 APK 的前提。

## 4. 构建方式

原项目：`scripts/build-magpie-android.ps1`（Go 交叉编译两个 ABI）+ `scripts/build-apk.ps1`（Gradle 打 APK）。

- Gradle + AGP，`compileSdk = 36`，`minSdk = 36`，`targetSdk = 36`
- `abiFilters = ["arm64-v8a", "x86_64"]`
- `useLegacyPackaging = true`（二进制必须解压到磁盘才能 exec）
- `keepDebugSymbols += "**/libmagpiebun.so"`
- release 用 **debug 签名**（明摆着方便侧载，README 里也说了要发出去得换）
- Go 版本由 `upstream/go.mod` 钉住（1.26.8），NDK r28c（作者说 r29 才有 API 36 的 wrapper，实际 r30 已经支持到 37）

APK 体积 268 MB：两个架构的 Go 二进制 98 MB + 两个架构的 Bun 176 MB。

## 5. 目标 Android 版本

`minSdk = compileSdk = targetSdk = 36`（Android 16）。作者在 README 里明说「往下调低也可以，壳子代码没用到更新的 API」。

用到的平台能力：前台服务（`specialUse`）、`RECEIVE_BOOT_COMPLETED`、`REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`、`ACCESS_LOCAL_NETWORK`（Android 16 新增，为 17 做准备）、`POST_NOTIFICATIONS`、`networkSecurityConfig`（只允许 loopback 明文）。

## 6. 对重构意味着什么

1. **壳可以完整重写**，代价低（5 个类），收益明确（少一个进程、少一套看门狗、日志统一）。
2. **网关必须划范围**。30 万行里，「agent 指向 3425 端口」这条路上真正会被用到的，是：协议互译 + 模型路由 + 密钥 + 配置页。其余（插件、会话、Library、订阅账号、余额）是 magpie 作为桌面产品的外围能力。
3. **`nogui` 的存在是个信号**：上游自己就承认「Android 上只需要网关 + 网页」，桌面那套（Wails、托盘、TUI）是可选层。Rust 版顺着这条线切。
4. **Bun 是原项目体积失控的唯一原因**。它只服务于 JS 插件；不做插件，APK 立刻回到 10 MB 量级。

具体方案见 [PLAN.md](PLAN.md)，落地后的差异清单见仓库根 `README.md` 的「与原项目的差异」。
