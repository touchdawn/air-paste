# Air Paste Handoff

Last updated: 2026-06-06

This document is for the next coding session. It summarizes the current repo state, what is already working, what is intentionally still MVP-grade, and the recommended next steps.

## Current Repository State

Branch: `main`

Recent commits:

- `f692f31 feat: let agents confirm pairing codes`
- `eeea033 feat: sign peer file download requests`
- `d0e5b5d feat: bind peer downloads to device request headers`
- `6f77287 feat: add authenticated peer file transfer`
- `11b7ec2 feat: add server and windows agent MVP`

The workspace currently contains:

- `crates/airpaste-core`: shared IDs and domain models.
- `crates/airpaste-protocol`: REST and WebSocket DTOs.
- `crates/airpaste-server`: Axum control-plane server using `redb`.
- `crates/airpaste-agent`: Windows agent MVP with text sync, file manifest, peer file server, file download, hotkey paste, Ed25519 device identity.

## Toolchain Notes

This Windows machine does not have the MSVC linker installed. Use the GNU Rust toolchain plus the portable WinLibs toolchain under `tools/winlibs`.

Use this PATH before Cargo commands:

```powershell
$env:PATH = "D:\ep\air-paste\tools\winlibs\mingw64\bin;$env:PATH"
```

Main commands:

```powershell
cargo +stable-x86_64-pc-windows-gnu check
cargo +stable-x86_64-pc-windows-gnu build -p airpaste-agent -p airpaste-server
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-agent.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-agent.ps1 -Bind 127.0.0.1:18082 -AuthToken airpaste-smoke-secret
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-server.ps1
```

Known harmless warning:

- Cargo may warn that hard linking in the incremental compilation cache failed. This is caused by the current filesystem behavior and Cargo falls back to copying files.

If network access is needed and direct access fails, the local proxy is usually:

```text
http://127.0.0.1:7897
```

## Current Server Behavior

The server supports:

- `GET /health`, `GET /v1/health`
- `POST /v1/devices`, `GET /v1/devices`
- `POST /v1/pair/start`
- `POST /v1/pair/confirm`
- `POST /v1/clips`
- `GET /v1/clips/latest`
- `GET /v1/clips/history`
- `GET /v1/clips/{clip_id}`
- `DELETE /v1/clips/{clip_id}`
- `POST /v1/relay/sessions`
- `GET /v1/ws`

Auth:

- `--auth-token <secret>` or `AIRPASTE_AUTH_TOKEN=<secret>` enables Bearer-token protection.
- `/health` and `/v1/health` stay public.
- Other REST and WebSocket routes require `Authorization: Bearer <secret>`.
- Sensitive REST routes and WebSocket upgrade also require a registered trusted `x-airpaste-device-id` plus an Ed25519 request signature.
- REST/WS request signatures use `x-airpaste-signature-alg`, `x-airpaste-timestamp`, `x-airpaste-nonce`, `x-airpaste-body-sha256`, and `x-airpaste-signature`.
- The signature message covers HTTP method, path/query, device ID, timestamp, nonce, and body SHA-256. The server rejects stale timestamps and replayed nonces.

Pairing/trust:

- The first registered device in a fresh DB is automatically trusted for bootstrap.
- Later devices register as untrusted.
- A non-first agent can confirm pairing by starting with `--pair-code <code>`.
- Pairing code creation is still API-only through `POST /v1/pair/start`, but that route now requires a trusted request device.
- Untrusted devices may register and confirm pairing, but cannot list devices, create/read clips, list history, open WebSocket sync, or create relay sessions until trusted.
- `POST /v1/devices` registration and `POST /v1/pair/confirm` remain usable by untrusted devices.

## Current Windows Agent Behavior

Agent state file now stores:

- `device_id`
- `device_private_key`

The agent:

- Generates an Ed25519 device identity on first run.
- Registers the device public key with the server.
- Reuses the saved private key on later runs.
- Can confirm pairing with `--pair-code <code>`.
- Publishes and applies text clipboard clips.
- Publishes file clipboard manifests from Windows `CF_HDROP`.
- Runs a peer HTTP server on `--peer-bind`, default `127.0.0.1:17390`.
- Receives remote file manifests and records them as pending by default.
- Downloads pending files on `Ctrl+Shift+V`, writes downloaded cache paths to Windows file clipboard, then sends normal paste.

Useful agent flags:

- `--server-url`
- `--auth-token`
- `--pair-code`
- `--state-path`
- `--peer-bind`
- `--peer-public-url`
- `--cache-dir`
- `--max-file-count`
- `--max-total-file-bytes`
- `--transfer-token-ttl-secs`
- `--auto-apply-files=true`
- `--auto-paste-files=true`
- `--remote-paste-hotkey=false`
- `--publish-clipboard=false`
- `--apply-remote=false`

## Current File Transfer Security

File payloads do not go through the server in the normal MVP path.

Source-side behavior:

- Publishes a `FileClip` manifest to the server.
- Registers local original file paths under a short-lived `transfer_token`.
- The token TTL defaults to 600 seconds.
- Each file index can be downloaded once.
- The peer file endpoint is:

```text
GET /v1/files/{transfer_token}/{index}
```

Requester-side behavior:

- Downloads from `source_peer_url`.
- Verifies each downloaded file byte count matches the manifest `FileEntry.size` before writing it into the local cache.
- Adds these peer request headers:
  - `x-airpaste-clip-id`
  - `x-airpaste-source-device-id`
  - `x-airpaste-requester-device-id`
  - `x-airpaste-signature-alg`
  - `x-airpaste-signature`
- Signs the peer request with the receiver's Ed25519 private key.

Source-side peer verification:

- Checks the request matches the local grant's clip ID, source device ID, token, and index.
- Checks requester is not the source device.
- Looks up trusted device public keys from the server at manifest publication time.
- Verifies the Ed25519 signature before reading the file.

Smoke coverage:

- Text publish/apply.
- File manifest publish.
- File peer download.
- Local file clipboard write.
- Server auth token path.
- Trusted-device signed API guard path: missing signature returns `401`, untrusted signed request returns `403`, paired signed request is allowed, replayed nonce returns `401`.
- Peer unauthenticated request returns `401`.
- Repeated file index download returns `410`.

## Important MVP Limitations

Security and trust:

- Text clips are still inline plaintext placeholders, not real end-to-end encrypted payloads.
- The server can still store and return plaintext text clips.
- REST signatures currently use an in-memory nonce cache, so replay protection resets when the server restarts.
- There is no UI fingerprint comparison for device public keys.

Transfer:

- Peer file server streams file responses from disk instead of buffering entire files in memory.
- Directories are represented in the manifest but skipped by transfer.
- There is no recursive directory copy.
- There is no resume, chunking, cryptographic checksum validation, or transfer progress.
- There is no mDNS/LAN discovery yet.
- Relay session metadata exists, but the relay data path is not implemented.

Platform:

- Clipboard, hotkey, paste, and file-drop implementation currently only work on Windows.
- macOS agent implementation is not started in code yet.
- No tray UI, settings UI, installer, or service/login-item packaging yet.

## Recommended Next Steps

### 1. Make Text Less Dangerous

Current text sync is functionally useful but security-weak.

Options:

- Temporarily gate text history behind config and short TTL.
- Add local sensitive-text filters.
- Add real E2EE content encryption using trusted device public keys.

### 2. Improve File Data Plane

Useful incremental improvements:

- Add max single-file size.
- Add SHA-256 while streaming.
- Add directory walking with file count and total-size caps.

### 3. Start macOS Agent

See `docs/MACOS_AGENT_PLAN.md`.

The best approach is to add macOS implementations behind the existing agent abstractions instead of starting a separate product.
