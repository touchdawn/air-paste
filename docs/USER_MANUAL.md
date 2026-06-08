# Air Paste 使用说明书

更新时间：2026-06-08

> 当前项目仍处于 MVP 开发阶段。本手册主体以“从源码编译并运行 CLI 程序”为准；现在也有一个**跨平台菜单栏/系统托盘图形界面**（`airpaste-tray`，内嵌 agent，支持窗口内配对/配置、开机自启），见文末“托盘应用”一节。安装包/签名公证暂未提供。

## 1. 简介

Air Paste 是一个用 Rust 编写的 Windows/macOS 跨设备剪贴板工具。它的目标是让你的多台可信设备共享剪贴板：在设备 A 复制文本或文件，在设备 B 接收并粘贴。

当前仓库包含两个主要可执行程序：

- `airpaste-server`：控制平面服务器。负责设备注册、配对、可信设备列表、剪贴板元数据、WebSocket 通知、文本历史，以及加密中继数据通道（只转发密文、不留存）。
- `airpaste-agent`：桌面客户端/代理。运行在每台 Mac 或 Windows 设备上，读取本机剪贴板、发布剪贴板变化、接收远端变化、提供点对点文件下载服务。

当前可用能力：

- 文本剪贴板同步：文本内容端到端加密后通过服务器同步。
- 文件剪贴板同步：服务器只保存文件清单，真实文件默认从源设备点对点下载。
- 设备配对：第一台注册到空数据库的设备自动可信，后续设备需要配对码。
- 远端文件粘贴热键：`Ctrl+Shift+V`。
- macOS 和 Windows 客户端均支持文本、文件清单、文件下载和本地剪贴板写入。

当前尚未完成或仍是 MVP 的能力：

- 有托盘 GUI（菜单栏/系统托盘 + 配置窗口 + 开机自启），但还没有安装包/系统服务/签名公证。
- 图片同步模型存在于协议里，但客户端当前主要实现文本和文件。
- 文件清单尚未端到端加密，服务器能看到文件名、大小、来源 peer URL 等元数据。
- 文件夹现在会**递归复制并保留目录结构**;空目录和符号链接不复制。
- Relay 中继数据通道已实现（端到端加密、服务器只转发不留存）：直连失败时接收端会**自动回退**到服务器中继；也可用 `--prefer-relay` 强制走中继。适合两台设备无法直连的情况。
- Windows 可以在文件下载后模拟 `Ctrl+V`；macOS 当前只写入 pasteboard 文件 URL，不自动模拟 `Cmd+V`。

## 2. 工作方式

Air Paste 分为控制平面和数据平面：

- 控制平面走 `airpaste-server`：设备注册、配对、WebSocket 通知、文本密文、文件清单。
- 数据平面优先走客户端之间的 peer HTTP 服务：源设备暴露 `GET /v1/files/{transfer_token}/{index}`，接收设备下载文件到本地缓存后写入本机剪贴板。

文本流程：

1. 源设备 agent 轮询本机剪贴板。
2. 发现新文本后，先做敏感内容过滤。
3. 文本端到端加密，发布到 server。
4. 其他可信设备通过 WebSocket 收到通知，拉取并解密文本。
5. 接收设备把文本写入本机剪贴板。

文件流程：

1. 源设备复制文件。
2. 源设备 agent 发布文件清单，不上传文件正文。
3. 接收设备记录“有一个远端文件剪贴板待处理”。
4. 用户在接收设备按 `Ctrl+Shift+V`，或开启自动应用文件。
5. 接收设备从源设备 peer 服务下载文件到本地缓存。
6. 接收设备把下载后的本地文件路径写入本机剪贴板。
7. Windows 会继续模拟 `Ctrl+V`；macOS 需要用户再按正常的 `Cmd+V` 或使用目标应用的粘贴动作。

## 3. 使用前准备

### 3.1 网络要求

所有客户端都必须能访问同一个 server URL，例如：

```text
http://192.168.50.199:8080
https://airpaste.example.com
```

如果 server 暴露在公网、DDNS 或不可信网络中，强烈建议启用 `--auth-token`，并优先用 HTTPS 反向代理。`airpaste-server` 本身只直接提供 HTTP 监听；如果需要 HTTPS，应由外部反向代理或网关终止 TLS。客户端使用 `https://` 时会自动把 WebSocket 地址转换为 `wss://`。

文件传输还要求接收设备能访问源设备的 peer 端口，默认是 `17390`：

- 同一局域网内，接收端会通过 mDNS（`_airpaste._tcp.local.`）自动发现源设备的 LAN 地址，并优先用它下载，无需手动配置 `--peer-public-url`。
- 如果 mDNS 不稳定、跨网段、VPN、端口转发或公网访问，请在源设备上显式配置 `--peer-public-url http://<本机可被对方访问的地址>:17390`；清单里的 `source_peer_url` 会作为 mDNS 的回退。
- 如果两台设备互相无法访问 peer 端口，接收端会**自动回退**到服务器加密中继（双方都只向服务器发起出站连接，服务器只转发密文、不留存）；也可用 `--prefer-relay` 强制走中继。

### 3.2 端口

常用端口：

- Server：`8080`，由 `--bind` 控制。
- Agent peer 文件服务：`17390`，由 `--peer-bind` 控制。

如果同一台机器上启动多个 agent 做测试，每个 agent 必须使用不同的 `--state-path`、`--cache-dir` 和 `--peer-bind`。

### 3.3 身份、状态和缓存

agent 第一次运行时会生成：

- Ed25519 设备签名密钥。
- X25519 文本加密密钥。
- 设备 ID。

这些信息保存在 agent 状态文件中。请把状态文件当作私密文件处理。

默认路径：

- macOS 状态文件：`~/Library/Application Support/AirPaste/agent.json`
- macOS 缓存目录：`~/Library/Caches/AirPaste`
- Windows 状态文件：当前工作目录下的 `.airpaste-agent.json`
- Windows 缓存目录：当前工作目录下的 `.airpaste-cache`

Windows 默认路径依赖启动时所在目录。实际使用时建议显式传入 `--state-path` 和 `--cache-dir`，避免从不同目录启动时生成多个设备身份。

## 4. 最小可用流程

下面是一个典型流程：

1. 在一台 Mac 或 Windows 上启动 `airpaste-server`。
2. 在第一台客户端启动 `airpaste-agent`。如果 server 数据库是空的，这台设备会自动成为可信设备。
3. 在可信设备上创建配对码。
4. 在第二台客户端启动 `airpaste-agent --pair-code <code>`。
5. 两台 agent 都保持运行。
6. 复制文本会自动同步。
7. 复制文件后，在接收端按 `Ctrl+Shift+V` 触发远端文件下载和粘贴准备。

如果 server 启用了 token，server 和所有 agent 命令都要使用同一个 token：

```text
--auth-token "<secret>"
```

或环境变量：

```text
AIRPASTE_AUTH_TOKEN=<secret>
```

## 5. macOS 客户端

### 5.1 能力状态

macOS agent 当前支持：

- 读取和写入文本 pasteboard。
- 读取和写入文件 URL pasteboard。
- 发布本机复制的文本和文件清单。
- 接收远端文本并写入本机 pasteboard。
- 接收远端文件清单，按 `Ctrl+Shift+V` 后下载文件并把文件 URL 写入 pasteboard。

macOS 当前不支持：

- 下载文件后自动模拟 `Cmd+V`。
- 托盘 UI、登录项、LaunchAgent 安装器。

### 5.2 编译

在项目根目录运行：

```bash
cargo build -p airpaste-agent
```

可执行文件会生成在：

```bash
target/debug/airpaste-agent
```

如果需要同时编译 server：

```bash
cargo build -p airpaste-server -p airpaste-agent
```

### 5.3 启动第一台 Mac 客户端

如果这是 server 空数据库上的第一台设备，它会自动被信任：

```bash
target/debug/airpaste-agent \
  --server-url http://<server-host>:8080 \
  --device-name "MacBook" \
  --auth-token "<secret-if-enabled>" \
  --peer-bind 0.0.0.0:17390 \
  --peer-public-url http://<this-mac-lan-ip>:17390
```

如果 server 没有启用 `--auth-token`，删除 `--auth-token` 这一行。

同一局域网且 mDNS 正常时，`--peer-public-url` 可以省略；但如果文件下载失败，优先显式设置它。

### 5.4 加入已有设备组

先在一台已可信设备上生成配对码：

```bash
target/debug/airpaste-agent \
  --server-url http://<server-host>:8080 \
  --auth-token "<secret-if-enabled>" \
  --create-pair-code \
  --pair-ttl-seconds 600 \
  --publish-clipboard=false \
  --apply-remote=false \
  --remote-paste-hotkey=false
```

命令会输出类似 JSON：

```json
{"code":"7Z3K9Q2A","expires_at":"2026-06-07T12:00:00Z"}
```

然后在新 Mac 上启动：

```bash
target/debug/airpaste-agent \
  --server-url http://<server-host>:8080 \
  --device-name "Mac Mini" \
  --auth-token "<secret-if-enabled>" \
  --pair-code "<code>" \
  --peer-bind 0.0.0.0:17390 \
  --peer-public-url http://<this-mac-lan-ip>:17390
```

如果可信设备使用了自定义 `--state-path`，创建配对码时也必须传入同一个 `--state-path`。

### 5.5 日常使用

文本：

- 保持 agent 运行。
- 在任意可信设备复制普通文本。
- 其他可信设备会自动把文本写入本机 pasteboard。
- 默认文本 clip 10 分钟过期，即 `--text-clip-ttl-secs 600`。

文件：

- 在源 Mac 的 Finder 或其他应用中复制文件。
- 接收 Mac 收到文件清单后不会立即下载，除非开启 `--auto-apply-files=true`。
- 在接收 Mac 按 `Ctrl+Shift+V`。
- agent 下载文件到缓存目录，把文件 URL 写入 pasteboard。
- 再按 `Cmd+V` 或使用目标应用的粘贴操作。

单次调试下载最新远端文件：

```bash
target/debug/airpaste-agent \
  --server-url http://<server-host>:8080 \
  --auth-token "<secret-if-enabled>" \
  --publish-clipboard=false \
  --apply-remote=false \
  --remote-paste-hotkey=false \
  --apply-latest-files-once
```

该命令会下载最新远端文件 clip，把下载结果写入 pasteboard，并以 JSON 打印下载后的本地路径。

### 5.6 macOS 客户端常用参数

- `--server-url`：server 地址，默认 `http://127.0.0.1:8080`。
- `--auth-token`：server token。server 启用 token 时必填。
- `--device-name`：设备显示名。
- `--pair-code`：加入已有设备组时使用。
- `--create-pair-code`：在可信设备上创建配对码。
- `--state-path`：设备身份状态文件。
- `--cache-dir`：远端文件下载缓存目录。
- `--peer-bind`：本机 peer 文件服务监听地址，默认 `0.0.0.0:17390`。
- `--peer-public-url`：写入文件清单的可访问 peer URL。
- `--publish-clipboard=false`：只接收，不发布本机剪贴板。
- `--apply-remote=false`：只发布，不应用远端剪贴板。
- `--remote-paste-hotkey=false`：禁用 `Ctrl+Shift+V` 监听。
- `--auto-apply-files=true`：收到文件清单后自动下载并写入 pasteboard。
- `--text-clip-ttl-secs 0`：文本 clip 不设置过期时间，主要用于调试。

### 5.7 macOS 验证脚本

自动 smoke：

```bash
scripts/smoke-agent-macos.sh
scripts/smoke-agent-macos.sh --auth-token airpaste-smoke-secret
```

交互式热键 smoke：

```bash
scripts/smoke-hotkey-macos.sh
```

脚本会准备一个待处理文件 clip，然后等待你手动按 `Ctrl+Shift+V`。

## 6. Windows 客户端

### 6.1 能力状态

Windows agent 当前支持：

- 读取和写入文本剪贴板。
- 读取和写入 Windows `CF_HDROP` 文件剪贴板。
- 发布本机复制的文本和文件清单。
- 接收远端文本并写入本机剪贴板。
- 接收远端文件清单，按 `Ctrl+Shift+V` 后下载文件、写入文件剪贴板，并模拟普通 `Ctrl+V`。

### 6.2 Windows 本机编译

第一次在 Windows 上准备工具链：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\setup-windows-toolchain.ps1
```

如果下载 Rust 或 WinLibs 需要代理：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\setup-windows-toolchain.ps1 -Proxy "http://127.0.0.1:7897"
```

每次编译前设置 MinGW PATH：

```powershell
$env:PATH = "$(Get-Location)\tools\winlibs\mingw64\bin;$env:PATH"
```

编译客户端：

```powershell
cargo +stable-x86_64-pc-windows-gnu build -p airpaste-agent
```

编译客户端和服务器：

```powershell
cargo +stable-x86_64-pc-windows-gnu build -p airpaste-agent -p airpaste-server
```

生成的客户端路径：

```powershell
.\target\debug\airpaste-agent.exe
```

### 6.3 从 macOS 交叉编译 Windows 客户端

如果你在 macOS 上生成 Windows 可执行文件：

```bash
rustup target add x86_64-pc-windows-gnu
brew install mingw-w64
scripts/cross-windows.sh build
```

生成路径：

```text
target/x86_64-pc-windows-gnu/debug/airpaste-agent.exe
target/x86_64-pc-windows-gnu/debug/airpaste-server.exe
```

注意：Windows 剪贴板、全局热键和模拟粘贴必须在真实 Windows 会话中测试。

### 6.4 启动第一台 Windows 客户端

```powershell
.\target\debug\airpaste-agent.exe `
  --server-url http://<server-host>:8080 `
  --device-name "Windows PC" `
  --auth-token "<secret-if-enabled>" `
  --state-path .\.airpaste-agent.json `
  --cache-dir .\.airpaste-cache `
  --peer-bind 0.0.0.0:17390 `
  --peer-public-url http://<this-windows-lan-ip>:17390
```

如果 server 没有启用 `--auth-token`，删除 `--auth-token` 这一行。

Windows 防火墙如果提示是否允许网络访问，请允许本机 peer 端口的入站访问；否则其他设备可能无法下载这台 Windows 机器上复制的文件。

### 6.5 加入已有设备组

在已有可信设备上创建配对码：

```powershell
.\target\debug\airpaste-agent.exe `
  --server-url http://<server-host>:8080 `
  --auth-token "<secret-if-enabled>" `
  --state-path .\.airpaste-agent.json `
  --create-pair-code `
  --pair-ttl-seconds 600 `
  --publish-clipboard=false `
  --apply-remote=false `
  --remote-paste-hotkey=false
```

在新 Windows 设备上启动：

```powershell
.\target\debug\airpaste-agent.exe `
  --server-url http://<server-host>:8080 `
  --device-name "Workstation" `
  --auth-token "<secret-if-enabled>" `
  --pair-code "<code>" `
  --state-path .\.airpaste-agent.json `
  --cache-dir .\.airpaste-cache `
  --peer-bind 0.0.0.0:17390 `
  --peer-public-url http://<this-windows-lan-ip>:17390
```

### 6.6 日常使用

文本：

- 保持 agent 运行。
- 在一台可信设备复制普通文本。
- 其他可信设备会自动收到并写入本机剪贴板。

文件：

- 在源设备复制一个或多个文件。
- 接收 Windows 设备收到文件清单后默认只记录为 pending。
- 在目标应用处于前台时按 `Ctrl+Shift+V`。
- agent 下载文件到缓存目录，写入 Windows 文件剪贴板，然后模拟 `Ctrl+V`。

如果使用 `--auto-apply-files=true` 让 agent 在收到文件清单后自动下载，但不希望自动下载后继续模拟粘贴，可以使用：

```powershell
--auto-paste-files=false
```

热键触发路径在 Windows 当前固定会在下载后模拟粘贴；自动应用文件路径是否模拟粘贴由 `--auto-paste-files` 控制。

### 6.7 Windows RDP 注意事项

如果通过 Remote Desktop 连接 Windows 机器，RDP 的 `rdpclip.exe` 可能会双向同步剪贴板，导致：

- agent 写入 Windows 剪贴板后很快被 RDP 覆盖。
- 复制文本被远程桌面回传，形成发布循环。
- `Get-Clipboard` 看到的内容与本机真实交互不一致。

验证 Air Paste 时，建议在 mstsc 的“本地资源”中关闭 Clipboard 重定向，或使用真实本地会话。

### 6.8 Windows 验证脚本

先确保已编译 server 和 agent，然后运行：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-agent.ps1
```

带 token：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-agent.ps1 -AuthToken airpaste-smoke-secret
```

验证已运行的 server：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-server.ps1 -BaseUrl http://127.0.0.1:8080
```

## 7. macOS 服务器端

### 7.1 编译

在项目根目录运行：

```bash
cargo build -p airpaste-server
```

可执行文件：

```bash
target/debug/airpaste-server
```

### 7.2 前台启动

不启用 token，仅适合本机或受信任局域网调试：

```bash
target/debug/airpaste-server \
  --bind 0.0.0.0:8080 \
  --db ./airpaste.redb
```

启用 token：

```bash
target/debug/airpaste-server \
  --bind 0.0.0.0:8080 \
  --db ./airpaste.redb \
  --auth-token "<secret>"
```

也可以使用环境变量，避免把 token 放进命令行历史：

```bash
AIRPASTE_BIND=0.0.0.0:8080 \
AIRPASTE_DB=./airpaste.redb \
AIRPASTE_AUTH_TOKEN="<secret>" \
target/debug/airpaste-server
```

健康检查：

```bash
curl http://127.0.0.1:8080/health
```

`/health` 和 `/v1/health` 始终公开；其他 API 在启用 token 后需要 Bearer token 和可信设备签名。

### 7.3 后台运行

当前仓库没有提供正式 LaunchAgent 配置。开发阶段可以先用终端、tmux、screen 或 `nohup`：

```bash
nohup target/debug/airpaste-server \
  --bind 0.0.0.0:8080 \
  --db ./airpaste.redb \
  --auth-token "<secret>" \
  > airpaste-server.log 2>&1 &
```

### 7.4 macOS 防火墙

如果其他设备无法访问 server：

- 确认 server 监听的是 `0.0.0.0:8080`，不是只监听 `127.0.0.1`。
- 确认使用的是这台 Mac 的局域网 IP 或域名。
- 在 macOS 防火墙或路由器上允许 `8080` 入站。

## 8. Windows 服务器端

### 8.1 编译

准备工具链：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\setup-windows-toolchain.ps1
```

设置 PATH：

```powershell
$env:PATH = "$(Get-Location)\tools\winlibs\mingw64\bin;$env:PATH"
```

编译 server：

```powershell
cargo +stable-x86_64-pc-windows-gnu build -p airpaste-server
```

可执行文件：

```powershell
.\target\debug\airpaste-server.exe
```

### 8.2 前台启动

不启用 token：

```powershell
.\target\debug\airpaste-server.exe --bind 0.0.0.0:8080 --db .\airpaste.redb
```

启用 token：

```powershell
.\target\debug\airpaste-server.exe --bind 0.0.0.0:8080 --db .\airpaste.redb --auth-token "<secret>"
```

或使用环境变量：

```powershell
$env:AIRPASTE_BIND = "0.0.0.0:8080"
$env:AIRPASTE_DB = ".\airpaste.redb"
$env:AIRPASTE_AUTH_TOKEN = "<secret>"
.\target\debug\airpaste-server.exe
```

健康检查：

```powershell
Invoke-RestMethod http://127.0.0.1:8080/health
```

### 8.3 后台启动

当前仓库没有提供 Windows Service 安装器。开发阶段可以用 PowerShell 启动后台进程：

```powershell
Start-Process `
  -FilePath .\target\debug\airpaste-server.exe `
  -ArgumentList @("--bind", "0.0.0.0:8080", "--db", ".\airpaste.redb", "--auth-token", "<secret>")
```

如果需要长期运行，后续应补 Windows Service、计划任务或外部进程管理配置。

### 8.4 Windows 防火墙

如果其他设备无法访问 Windows server：

- 确认 server 监听 `0.0.0.0:8080`。
- 确认客户端使用的是 Windows 机器的局域网 IP 或域名。
- 在 Windows Defender 防火墙中允许 `airpaste-server.exe` 或 TCP `8080` 入站。

## 9. 配对和信任模型

空数据库启动后的第一台注册设备会被自动标记为 trusted，用于引导配对。

后续设备流程：

1. 新设备第一次连接 server，会注册设备身份，但默认不可信。
2. 已可信设备创建配对码。
3. 新设备带 `--pair-code <code>` 启动。
4. server 确认配对后，新设备变为 trusted。

配对码创建命令必须由可信设备发起。普通使用建议只通过 agent CLI 创建配对码，不直接调用 REST API。

重置设备身份：

- 停止 agent。
- 删除本机 agent 状态文件。
- 下次启动会生成新身份，并需要重新配对。

注意：当前没有面向用户的设备删除/撤销 UI。server 数据库中的旧设备记录需要后续补管理能力。

## 10. 常用参数速查

### 10.1 Server

| 参数 | 环境变量 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `--bind` | `AIRPASTE_BIND` | `0.0.0.0:8080` | server 监听地址 |
| `--db` | `AIRPASTE_DB` | `airpaste.redb` | redb 数据库路径 |
| `--auth-token` | `AIRPASTE_AUTH_TOKEN` | 空 | 启用 Bearer token 保护 |

### 10.2 Agent

| 参数 | 环境变量 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `--server-url` | `AIRPASTE_SERVER` | `http://127.0.0.1:8080` | server 地址 |
| `--auth-token` | `AIRPASTE_AUTH_TOKEN` | 空 | server token |
| `--device-name` | `AIRPASTE_DEVICE_NAME` | 平台默认名 | 设备名 |
| `--state-path` | `AIRPASTE_STATE` | 平台默认路径 | 身份状态文件 |
| `--pair-code` | `AIRPASTE_PAIR_CODE` | 空 | 加入配对 |
| `--create-pair-code` | `AIRPASTE_CREATE_PAIR_CODE` | `false` | 创建配对码并退出 |
| `--pair-ttl-seconds` | `AIRPASTE_PAIR_TTL_SECONDS` | server 默认 | 配对码有效期 |
| `--print-latest-clip` | `AIRPASTE_PRINT_LATEST_CLIP` | `false` | 打印最新 clip 并退出 |
| `--publish-text-once` | `AIRPASTE_PUBLISH_TEXT_ONCE` | 空 | 发布一次文本并退出 |
| `--apply-latest-files-once` | `AIRPASTE_APPLY_LATEST_FILES_ONCE` | `false` | 下载最新远端文件并退出 |
| `--poll-ms` | `AIRPASTE_POLL_MS` | `750` | 剪贴板轮询间隔 |
| `--text-clip-ttl-secs` | `AIRPASTE_TEXT_CLIP_TTL_SECS` | `600` | 文本 clip 过期秒数，`0` 表示不过期 |
| `--filter-sensitive-text` | `AIRPASTE_FILTER_SENSITIVE_TEXT` | `true` | 是否过滤明显敏感文本 |
| `--max-text-clip-bytes` | `AIRPASTE_MAX_TEXT_CLIP_BYTES` | `131072` | 自动发布文本最大字节数 |
| `--peer-bind` | `AIRPASTE_PEER_BIND` | `0.0.0.0:17390` | peer 文件服务监听地址 |
| `--peer-public-url` | `AIRPASTE_PEER_PUBLIC_URL` | 根据 bind 生成 | 其他设备访问本机 peer 的 URL |
| `--cache-dir` | `AIRPASTE_CACHE_DIR` | 平台默认路径 | 下载文件缓存目录 |
| `--max-file-count` | `AIRPASTE_MAX_FILE_COUNT` | `1000` | 单次文件清单最大条目数 |
| `--max-single-file-bytes` | `AIRPASTE_MAX_SINGLE_FILE_BYTES` | `10737418240` | 单文件最大大小，默认 10 GiB |
| `--max-total-file-bytes` | `AIRPASTE_MAX_TOTAL_FILE_BYTES` | `10737418240` | 单次总大小上限，默认 10 GiB |
| `--transfer-token-ttl-secs` | `AIRPASTE_TRANSFER_TOKEN_TTL_SECS` | `600` | peer 下载 token 有效期 |
| `--publish-clipboard` | `AIRPASTE_PUBLISH_CLIPBOARD` | `true` | 是否发布本机剪贴板变化 |
| `--apply-remote` | `AIRPASTE_APPLY_REMOTE` | `true` | 是否应用远端剪贴板变化 |
| `--prefer-relay` | `AIRPASTE_PREFER_RELAY` | `false` | 文件改走服务器加密中继，而非直连 |
| `--remote-paste-hotkey` | `AIRPASTE_REMOTE_PASTE_HOTKEY` | `true` | 是否启用 `Ctrl+Shift+V` |
| `--auto-apply-files` | `AIRPASTE_AUTO_APPLY_FILES` | `false` | 收到文件清单后是否自动下载 |
| `--auto-paste-files` | `AIRPASTE_AUTO_PASTE_FILES` | `false` | 自动下载后是否模拟粘贴 |

## 11. 安全和隐私说明

已实现：

- 文本内容使用 X25519 + XChaCha20-Poly1305 端到端加密。
- 文本的每个 clip 使用随机内容密钥，并为每台可信设备分别包裹密钥。
- server 只保存文本密文、nonce 和 wrapped keys。
- 敏感文本过滤默认开启，会跳过私钥、JWT、Bearer token、常见 provider token、secret-like assignment、一次性验证码样式数字、银行卡样式数字和过大的文本。
- REST 和 WebSocket 的敏感 API 需要可信设备签名。
- peer 文件下载请求需要可信设备签名。
- peer 文件下载会校验大小和 SHA-256。

仍需注意：

- 文件清单当前未加密，server 能看到文件名、大小、来源设备和 peer URL。
- 文本长度会通过 `utf8_len` 暴露。
- 旧 plaintext 文本 clip 仍会被兼容读取并警告。
- server 的 REST nonce replay 缓存是内存态，server 重启后会重置。
- 没有图形化公钥指纹比对。
- 源设备 peer token 默认 600 秒有效，且每个文件 index 默认只能下载一次；失败后通常需要重新复制文件生成新清单。

## 12. 故障排查

### 12.1 客户端连不上 server

检查：

- `server-url` 是否用了其他设备可访问的 IP/域名，而不是 `127.0.0.1`。
- server 是否监听 `0.0.0.0:8080`。
- 防火墙是否允许 server 端口。
- 启用 token 时，agent 是否传了同一个 `--auth-token`。
- server 健康检查是否成功：

```bash
curl http://<server-host>:8080/health
```

或 Windows：

```powershell
Invoke-RestMethod http://<server-host>:8080/health
```

### 12.2 新设备无法读取 clip，返回 403

通常说明设备已注册但尚未 trusted：

- 在可信设备上重新创建配对码。
- 新设备用 `--pair-code <code>` 启动。
- 确认创建配对码时使用的是可信设备的状态文件。

### 12.3 文本没有同步

检查：

- 两端 agent 是否都在运行。
- 接收端是否设置了 `--apply-remote=false`。
- 发布端是否设置了 `--publish-clipboard=false`。
- 文本是否被敏感内容过滤器跳过。
- 文本是否超过 `--max-text-clip-bytes`。
- Windows RDP 是否开启了剪贴板重定向。

调试时可以临时关闭敏感过滤：

```bash
--filter-sensitive-text=false
```

### 12.4 文件清单出现了，但文件下载失败

检查：

- 源设备 agent 是否仍在运行。
- 是否超过 `--transfer-token-ttl-secs`。
- 是否已经下载过同一个文件 index。
- 接收设备能否访问源设备 `--peer-public-url`。
- 防火墙是否允许源设备 peer 端口。
- 如果 mDNS 发现不可靠，源设备显式设置 `--peer-public-url http://<source-lan-ip>:17390`。
- 两端的 `--max-single-file-bytes`、`--max-total-file-bytes` 是否允许该文件大小。

### 12.5 `Ctrl+Shift+V` 没反应

检查：

- agent 是否启用了 `--remote-paste-hotkey=true`。
- 是否有其他应用占用了同一个全局热键。
- macOS 下该热键只负责下载并写入 pasteboard，不自动 `Cmd+V`。
- Windows 下目标应用是否在前台，且没有被权限边界阻止输入模拟。

### 12.6 多个 agent 在同一机器测试时端口冲突

为每个 agent 指定不同路径和端口：

```bash
target/debug/airpaste-agent \
  --state-path /tmp/airpaste-a.json \
  --cache-dir /tmp/airpaste-a-cache \
  --peer-bind 127.0.0.1:17391

target/debug/airpaste-agent \
  --state-path /tmp/airpaste-b.json \
  --cache-dir /tmp/airpaste-b-cache \
  --peer-bind 127.0.0.1:17392
```

## 13. 开发验证命令

通用检查：

```bash
cargo check
cargo test
```

macOS：

```bash
scripts/smoke-agent-macos.sh
scripts/smoke-agent-macos.sh --auth-token airpaste-smoke-secret
scripts/smoke-hotkey-macos.sh
```

macOS 交叉检查 Windows：

```bash
scripts/cross-windows.sh
scripts/cross-windows.sh build
```

Windows：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-agent.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-agent.ps1 -AuthToken airpaste-smoke-secret
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-server.ps1 -BaseUrl http://127.0.0.1:8080
```

## 14. 托盘应用（菜单栏 / 系统托盘 UI）

`airpaste-tray` 是一个跨平台图形界面，把 agent 内嵌进来，常驻 macOS 菜单栏 / Windows 系统托盘，
不占 Dock、不占任务栏。它接受与 agent 相同的命令行参数，也可以**完全在窗口里配置**（不必带参数启动）。

### 14.1 运行

macOS：

```bash
cargo run -p airpaste-tray                       # 用窗口里保存的配置（或默认值）启动
cargo run -p airpaste-tray -- --server-url http://<主机:端口> --pair-code <配对码>
```

Windows（先设置 WinLibs 工具链 PATH，见第 6.2 节）：

```powershell
cargo +stable-x86_64-pc-windows-gnu run -p airpaste-tray
```

首次启动若没有配置、也没连上服务器，窗口会显示红色 `✕ 连接失败`。

### 14.2 窗口功能

- **连接状态**：`● 已连接`（绿）/ `✕ 连接失败:…`（红，附原因）/ `○ 连接中…`。
- **设备 / 设备 ID**。
- **隔离模式**复选框：勾上后远端文本进“收件箱”而不覆盖系统剪贴板，配合 `Ctrl+Shift+C` / `Ctrl+Shift+V`。
- **开机自启**复选框：macOS 写 `~/Library/LaunchAgents` 的 LaunchAgent；Windows 写注册表 `HKCU\…\Run`。
- **设置 / 连接** 面板：填**服务器地址 / 配对码 / 认证令牌**，点 **保存并连接**。配置会持久化到
  `~/Library/Application Support/AirPaste/tray-config.json`（macOS）或 `%APPDATA%\AirPaste\tray-config.json`
  （Windows），下次启动自动连接；配对码用过一次后会自动清掉。保存时会重启应用以应用新配置。
- **收件箱**：隔离模式下收到的最近若干条文本，逐条可“复制”。
- 关闭窗口只是隐藏到托盘；托盘菜单的“退出 AirPaste”才真正退出，“显示 AirPaste”重新打开窗口。

显式命令行参数 / 环境变量优先于窗口里保存的配置（方便脚本化与冒烟测试）。

### 14.3 打包与自启

macOS 打成 `.app`（菜单栏 accessory，无 Dock 图标）：

```bash
scripts/bundle-macos.sh          # 生成 dist/AirPaste.app
open dist/AirPaste.app           # 或 cp -R dist/AirPaste.app /Applications/
```

Windows 安装到稳定路径（让“开机自启”的注册表项指向不随重新编译而变的位置）：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\install-windows.ps1   # 拷到 %LOCALAPPDATA%\AirPaste
```

启动安装后的那个 exe，再在窗口里勾“开机自启”，注册表项就会指向该稳定路径。

### 14.4 托盘验证脚本

```powershell
# Windows：托盘端到端连接（CLI 参数驱动）
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-tray-connect.ps1
# Windows：托盘读取持久化配置文件并连接
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-tray-config.ps1
```
