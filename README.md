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
- [docs/SESSION_HANDOFF.md](docs/SESSION_HANDOFF.md)
- [docs/MACOS_AGENT_PLAN.md](docs/MACOS_AGENT_PLAN.md)

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

For a DDNS/private deployment, start the server with `--auth-token <secret>` or `AIRPASTE_AUTH_TOKEN=<secret>`. Health checks stay public; all other REST and WebSocket APIs require `Authorization: Bearer <secret>`. Agents use the same value with `--auth-token <secret>`.

Sensitive server APIs also require the request device to be trusted and to prove possession of its Ed25519 private key. Agents sign REST and WebSocket requests with `x-airpaste-device-id`, `x-airpaste-signature-alg`, `x-airpaste-timestamp`, `x-airpaste-nonce`, `x-airpaste-body-sha256`, and `x-airpaste-signature`. The first registered device in a fresh database is trusted for bootstrap; later devices must be paired before they can list devices, create/read clips, open WebSocket sync, or create relay sessions. Device registration and pair confirmation remain available to untrusted devices.

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
.\target\debug\airpaste-agent.exe --server-url http://127.0.0.1:8080 --state-path .\.airpaste-agent-a.json --device-name "PC A" --auth-token "<secret-if-server-enabled-it>"
```

To join a non-first device, create a pairing code through `POST /v1/pair/start` from an already trusted device, then start the new agent with `--pair-code <code>`. The first registered device in a fresh database is trusted automatically for bootstrap.

Current agent scope:

- Windows text clipboard publish/apply.
- Windows file clipboard manifest publish via `CF_HDROP`.
- MVP file payload download from source agent peer HTTP service into receiver cache.
- Downloaded files are written back to the system clipboard as a file drop list.
- Windows remote file paste hotkey: `Ctrl+Shift+V`.

File transfer MVP notes:

- The source agent exposes `GET /v1/files/{transfer_token}/{index}` on its `--peer-bind` address.
- The file manifest includes `source_peer_url`; use `--peer-public-url` when another device cannot reach the bind address literally.
- Peer file requests must include `x-airpaste-clip-id`, `x-airpaste-source-device-id`, `x-airpaste-requester-device-id`, and an Ed25519 `x-airpaste-signature`; the source agent verifies the requester against trusted device public keys from the server.
- The peer transfer token has a local TTL, defaults to 600 seconds, and each file index can be downloaded once.
- File manifest publication is limited by `--max-file-count`, `--max-single-file-bytes`, and `--max-total-file-bytes`.
- Receivers reject remote file manifests whose regular files exceed `--max-single-file-bytes` and verify downloaded byte counts against the manifest before writing files into the cache.
- Only regular files are downloaded in this MVP. Directories are announced in the manifest but skipped by transfer.
- Downloaded files are written under `--cache-dir/<transfer_token>/`.
- By default, a remote file manifest is only recorded as pending. Press `Ctrl+Shift+V` on the receiver to download the latest pending files, write them to the local clipboard, and send a normal paste.
- `--auto-apply-files=true` downloads remote files as soon as the manifest arrives. This is mainly useful for smoke tests and debugging.
- `--auto-paste-files=true` sends `Ctrl+V` to the current foreground app after an automatic file apply, so keep it disabled unless the receiver is intentionally focused on the target app.

Smoke test:

```powershell
.\scripts\smoke-agent.ps1
```

