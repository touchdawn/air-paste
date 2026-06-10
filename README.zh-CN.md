# Air Paste

[English](README.md) | 简体中文

Air Paste 是一个基于 Rust 的 Windows / macOS 共享剪贴板工具：在设备 A 复制文本或文件，在设备 B 按一个快捷键就能粘贴。文本端到端加密；文件在局域网内点对点传输（无法直连时自动回退到加密的服务器中继），服务器永远不存储文件内容。

## 日常怎么用

在每台设备上运行托盘应用（`airpaste-tray`，常驻 macOS 菜单栏 / Windows 系统托盘）。然后：

- **发送**：正常复制（`Cmd+C` / `Ctrl+C`），再按 **`Option+C`**（macOS）/ **`Alt+C`**（Windows），内容就发布到了其他设备。
- **接收**：聚焦目标应用，按 **`Option+V`** / **`Alt+V`** —— 最近一条文本会被粘贴；如果最近收到的是文件，则下载并粘贴文件。
- 不想用快捷键也可以全在窗口里完成：输入文字点「发送」、把文件拖进窗口发送、在收件箱点「复制」/「下载」接收。

上面描述的就是**隔离模式**的用法（托盘应用的默认模式，也是我们推荐的模式），详见下一节。

## 剪贴板模式：隔离 vs 系统（建议使用隔离模式）

Agent 有两种剪贴板工作模式，由托盘窗口里的「隔离模式」复选框或 CLI 参数 `--clipboard-mode=isolated|system` 控制。**托盘 GUI 默认隔离模式；CLI agent 默认系统模式。**

### 隔离模式（推荐）

AirPaste 与系统剪贴板完全解耦，收发都由你显式触发：

- **发送是显式的**：先正常 `Cmd+C` / `Ctrl+C` 复制，再按 `Option+C` / `Alt+C`，当前剪贴板内容才会端到端加密发布出去。与上次发送内容相同时会自动跳过——连按两次只发一条，也不会误发剪贴板里的陈旧内容。
- **接收是显式的**：收到的远端文本只进 AirPaste **收件箱**（可在窗口里查看、点「复制」），**不会**自动覆盖你的系统剪贴板。按 `Option+V` / `Alt+V` 时才把最近一条文本粘贴到当前应用——粘贴时临时借用系统剪贴板，完成后自动还原；若最近到达的是文件，则改为下载并粘贴文件。

推荐它的原因：

- **不外泄**：不是每次复制都被发出去，只有你按 `Option+C` / `Alt+C` 主动发送的内容才离开本机。系统模式下靠启发式的敏感内容过滤兜底，隔离模式从机制上就不依赖它。
- **不污染**：其他设备的复制动作永远不会悄悄覆盖你本机剪贴板里正要粘贴的东西。

### 系统模式

传统的「复制即同步」剪贴板：

- 本机每次复制文本都会自动发布（先经过敏感内容过滤：私钥、JWT、token、验证码、卡号样式等会被跳过）；
- 收到的远端文本会自动写入本机系统剪贴板，直接 `Cmd+V` / `Ctrl+V` 即可。

更省事，但代价是：每次复制默认外发（过滤只是启发式的），且远端内容可能在你不知情时覆盖本机剪贴板。适合两台都是自己的、且在可信局域网内的设备。

两种模式下**文件**的行为一致：收到文件清单只记为待处理，按 `Option+V` / `Alt+V`（或在收件箱点「下载」）才真正下载。

切换方式：托盘窗口勾选 / 取消「隔离模式」，或 CLI 启动时指定 `--clipboard-mode=isolated` / `--clipboard-mode=system`。

> macOS 权限提示：`Option+V`（向其他应用模拟粘贴）需要辅助功能授权；`Option+C` 只读取剪贴板，不需要。

## 快速开始（纯 GUI，不碰命令行）

准备两台设备 A、B，A 兼作服务器。

先在每台设备上编译并启动托盘应用（暂未提供安装包）：

```bash
# macOS
scripts/bundle-macos.sh && open dist/AirPaste.app
```

```powershell
# Windows（需先准备工具链，见下文「编译」）
cargo +stable-x86_64-pc-windows-gnu run -p airpaste-tray
```

接下来全部在托盘窗口里操作：

1. **A**：打开设置面板，服务器地址保持默认 `http://127.0.0.1:14444`，勾选 **「本机作为服务器」**，点 **「保存并连接」**。A 是第一台设备，自动信任 → `● 已连接`。
2. **A**：点 **「生成配对码」**，记下 6 位数字码。
3. **B**：服务器地址填 A 的局域网地址（`http://<A的IP>:14444`），填入配对码，点 **「保存并连接」** → `● 已连接`。
4. 在一台设备复制后按 `Option+C` / `Alt+C` 发送，在另一台按 `Option+V` / `Alt+V` 粘贴。搞定。

> macOS：需要给托盘应用授权辅助功能（系统设置 → 隐私与安全性 → 辅助功能），否则粘贴快捷键无法向其他应用输入。想开机自启就在每台设备上勾选 **「开机自启」**。

完整教程 —— CLI 用法、配对细节、认证令牌、故障排查 —— 见 [docs/USER_MANUAL.md](docs/USER_MANUAL.md)。

## 编译

macOS：

```bash
cargo build -p airpaste-tray            # 托盘 GUI（内嵌 agent）
cargo build -p airpaste-server -p airpaste-agent   # CLI 服务器 + agent
```

Windows（首次运行会安装 Rust，并在 `tools/winlibs` 下下载便携版 WinLibs MinGW 工具链；网络可直连时省略 `-Proxy`）：

```powershell
.\scripts\setup-windows-toolchain.ps1 -Proxy "http://127.0.0.1:7897"
$env:PATH = "$(Get-Location)\tools\winlibs\mingw64\bin;$env:PATH"
cargo +stable-x86_64-pc-windows-gnu build -p airpaste-tray -p airpaste-server -p airpaste-agent
```

## 设计目标

- 服务器作为控制平面；
- 局域网 / 点对点直连作为首选数据平面；
- 端到端加密的流式中继作为可靠性兜底；
- 服务端默认不存储文件；
- 加密文本历史作为可选的便利功能；
- 使用显式的远程粘贴快捷键，保证 MVP 阶段文件粘贴的可靠性。

文档：

- [docs/USER_MANUAL.md](docs/USER_MANUAL.md) —— 完整使用说明书
- [docs/DESIGN.md](docs/DESIGN.md)
- [docs/SESSION_HANDOFF.md](docs/SESSION_HANDOFF.md)
- [docs/MACOS_AGENT_PLAN.md](docs/MACOS_AGENT_PLAN.md)

## Workspace 结构

- `crates/airpaste-core`：共享领域类型。
- `crates/airpaste-protocol`：REST 与 WebSocket DTO。
- `crates/airpaste-crypto`：端到端内容加密（X25519 + XChaCha20-Poly1305）。
- `crates/airpaste-server`：内嵌 `redb` 存储的 Axum 服务器。
- `crates/airpaste-agent`：负责文本同步与文件清单发布的 CLI agent。
- `crates/airpaste-tray`：内嵌 agent 的跨平台托盘 GUI。

## CLI 服务器与 agent（进阶 / 无界面部署）

托盘应用本身就能代跑服务器；只有无界面或脚本化部署才需要直接运行下面的命令。

运行服务器：

```powershell
cargo +stable-x86_64-pc-windows-gnu run -p airpaste-server -- --bind 0.0.0.0:14444 --db .\airpaste.redb
```

如果是 DDNS / 私有部署，启动服务器时加上 `--auth-token <secret>` 或设置 `AIRPASTE_AUTH_TOKEN=<secret>`。健康检查保持公开；其余所有 REST 和 WebSocket API 都要求 `Authorization: Bearer <secret>`。Agent 端用 `--auth-token <secret>` 传入同一个值。

敏感的服务器 API 还要求请求设备已被信任，并证明其持有对应的 Ed25519 私钥。Agent 使用 `x-airpaste-device-id`、`x-airpaste-signature-alg`、`x-airpaste-timestamp`、`x-airpaste-nonce`、`x-airpaste-body-sha256` 和 `x-airpaste-signature` 对 REST 与 WebSocket 请求签名。全新数据库中注册的第一台设备会被自动信任以完成引导；之后的设备必须先完成配对，才能列出设备、创建/读取剪贴内容、建立 WebSocket 同步或创建中继会话。设备注册和配对确认对未受信任的设备仍然开放。

常用端点：

- `GET /health`
- `POST /v1/devices`
- `GET /v1/devices`
- `POST /v1/devices/{device_id}/trust`
- `POST /v1/pair/start`
- `POST /v1/pair/confirm`
- `POST /v1/clips`
- `GET /v1/clips/latest`
- `GET /v1/clips/history`
- `POST /v1/relay/sessions`
- `GET /v1/relay/{session_id}/ws`
- `GET /v1/ws`

让 agent 连接本地服务器运行：

```powershell
.\target\debug\airpaste-agent.exe --server-url http://127.0.0.1:14444 --state-path .\.airpaste-agent-a.json --device-name "PC A" --auth-token "<secret-if-server-enabled-it>"
```

要加入非首台设备，先在已受信任的设备上通过 `POST /v1/pair/start` 创建配对码，然后用 `--pair-code <code>` 启动新 agent。或者，已受信任的设备也可以直接批准已注册的设备 —— 在托盘「设备」标签页中点击未信任设备旁的「信任」按钮，或在 CLI 中使用 `--trust-device <device-id>`。全新数据库中注册的第一台设备会被自动信任以完成引导。

当前 agent 功能范围：

- 文本剪贴内容端到端加密。Agent 在 Ed25519 身份之外另外生成一个 X25519 密钥并注册其公钥，每条剪贴内容用随机的单条密钥加密，再将该密钥为每台受信任设备封装。服务器只存储密文、临时公钥和 nonce。旧的明文剪贴内容在读取时仍会应用，但会给出警告。此功能之前注册的设备会自动重新注册以公布其加密密钥。
- Windows 文本剪贴板的发布 / 应用。
- 通过 `CF_HDROP` 发布 Windows 文件剪贴板清单。
- MVP 文件载荷从源 agent 的对等 HTTP 服务下载到接收方缓存。
- 下载完成的文件会以文件拖放列表的形式写回系统剪贴板。
- 远程粘贴快捷键：`Alt+V`（macOS 为 `Option+V`）。在隔离剪贴板模式下，`Alt+C` / `Option+C` 发布当前剪贴板内容。
- Agent 发布的文本剪贴内容默认在服务端有 600 秒 TTL。调试时可用 `--text-clip-ttl-secs 0` 关闭文本过期。
- 自动文本剪贴板发布默认会跳过明显的敏感内容，包括私钥、JWT、bearer token、平台令牌（`ghp_`、`github_pat_`、`sk-`）、形如密钥赋值的文本、疑似一次性验证码的数字、疑似信用卡号的数字，以及超过 `--max-text-clip-bytes` 的文本。调试时可用 `--filter-sensitive-text=false` 关闭。

文件传输 MVP 说明：

- 源 agent 在其 `--peer-bind` 地址上暴露 `GET /v1/files/{transfer_token}/{index}`，默认值为 `0.0.0.0:17390`，以便局域网内的对等设备访问。
- Agent 之间通过 mDNS 在局域网内互相发现（`_airpaste._tcp.local.`，TXT 记录中带 `device_id`）。接收方优先使用 mDNS 发现到的对等地址而非清单中的地址，所以在局域网内通常不需要 `--peer-public-url`。文件清单仍包含 `source_peer_url` 作为 mDNS 不可用时的兜底；该场景下请设置 `--peer-public-url`。
- 当直连 / 局域网传输不可行时，接收方加上 `--prefer-relay` 启动，通过服务器中介的加密中继拉取文件。两台设备都向外连接 `GET /v1/relay/{session_id}/ws`；源端在数据经过服务器之前就为接收方做了端到端加密（X25519 + XChaCha20-Poly1305），服务器只转发不透明帧，永远看不到明文。中继复用与直连路径相同的签名式对等文件授权。
- 对等文件请求必须携带 `x-airpaste-clip-id`、`x-airpaste-source-device-id`、`x-airpaste-requester-device-id` 和 Ed25519 签名 `x-airpaste-signature`；源 agent 会用从服务器获取的受信任设备公钥验证请求方。
- 对等传输令牌有本地 TTL，默认 600 秒，且每个文件索引只能下载一次。
- 文件清单发布受 `--max-file-count`、`--max-single-file-bytes` 和 `--max-total-file-bytes` 限制。
- 新的文件清单为普通文件附带小写十六进制 SHA-256。
- 接收方会拒绝普通文件超过 `--max-single-file-bytes` 的远程文件清单，将对等下载流式写入临时文件，并在写入缓存前校验下载字节数和 SHA-256。不含 SHA-256 的旧清单退化为只校验大小并给出警告。
- 本 MVP 只下载普通文件。目录会出现在清单中，但传输时会被跳过。
- 下载的文件写入 `--cache-dir/<transfer_token>/` 目录。
- 默认情况下，远程文件清单只会记录为待处理状态。在接收方按 `Alt+V`（macOS 为 `Option+V`）即可下载最新的待处理文件，写入本地剪贴板并发送一次普通粘贴。
- `--auto-apply-files=true` 会在清单到达时立即下载远程文件，主要用于冒烟测试和调试。
- `--apply-latest-files-once` 一次性下载最新的远程文件剪贴内容，把下载的文件引用写入本地剪贴板，以 JSON 形式打印下载路径后退出。便于 macOS 快捷键 / 剪贴板调试。
- `--auto-paste-files=true` 会在自动应用文件后向当前前台应用发送 `Ctrl+V`，除非接收方有意聚焦在目标应用上，否则请保持关闭。

## 冒烟测试

```powershell
.\scripts\smoke-agent.ps1
```

macOS 上：

```bash
scripts/smoke-agent-macos.sh
scripts/smoke-agent-macos.sh --auth-token airpaste-smoke-secret
scripts/smoke-hotkey-macos.sh
```

`smoke-hotkey-macos.sh` 是交互式的：它会准备一条待处理的文件剪贴内容，然后等待你按下远程粘贴快捷键（`Option+V`）。注意：脚本本身仍引用旧的 `Ctrl+Shift+V` 组合键，使用前需要更新。
