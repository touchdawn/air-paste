# Air Paste

Air Paste is a Rust-based shared clipboard for Windows and macOS.

The design goal is:

- server as control plane;
- LAN/direct peer transfer as the preferred data plane;
- end-to-end encrypted stream relay as the reliability fallback;
- no default server-side file storage;
- encrypted text history as an opt-in convenience;
- explicit remote paste hotkey for reliable MVP file paste.

Start here:

- [docs/DESIGN.md](docs/DESIGN.md)

Recommended first implementation step:

1. Create the Rust workspace layout described in `docs/DESIGN.md`.
2. Implement shared protocol/domain types.
3. Build the server health check and WebSocket skeleton.
4. Build the agent config and startup skeleton.

## Current MVP Server

The repository currently includes a runnable Rust control-plane server:

- `crates/airpaste-core`: shared domain types.
- `crates/airpaste-protocol`: REST and WebSocket DTOs.
- `crates/airpaste-server`: Axum server with embedded `redb` storage.
- `crates/airpaste-agent`: Windows agent MVP for text sync and file manifest publishing.

Run it with:

```powershell
.\scripts\setup-windows-toolchain.ps1 -Proxy "http://127.0.0.1:7897"
$env:PATH = "D:\ep\air-paste\tools\winlibs\mingw64\bin;$env:PATH"
cargo +stable-x86_64-pc-windows-gnu run -p airpaste-server -- --bind 0.0.0.0:8080 --db .\airpaste.redb
```

The setup script installs Rust and downloads a portable WinLibs MinGW toolchain under `tools/winlibs` for linking on Windows. Omit `-Proxy` when direct network access works.

Useful endpoints:

- `GET /health`
- `POST /v1/devices`
- `GET /v1/devices`
- `POST /v1/pair/start`
- `POST /v1/pair/confirm`
- `POST /v1/clips`
- `GET /v1/clips/latest`
- `GET /v1/clips/history`
- `POST /v1/relay/sessions`
- `GET /v1/ws`

Build both binaries with:

```powershell
$env:PATH = "D:\ep\air-paste\tools\winlibs\mingw64\bin;$env:PATH"
cargo +stable-x86_64-pc-windows-gnu build -p airpaste-server -p airpaste-agent
```

Run the agent against a local server:

```powershell
.\target\debug\airpaste-agent.exe --server-url http://127.0.0.1:8080 --state-path .\.airpaste-agent-a.json --device-name "PC A"
```

Current agent scope:

- Windows text clipboard publish/apply.
- Windows file clipboard manifest publish via `CF_HDROP`.
- File payload transfer and remote paste are not implemented yet.

Smoke test:

```powershell
.\scripts\smoke-agent.ps1
```

