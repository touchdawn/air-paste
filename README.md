# Air Paste

English | [简体中文](README.zh-CN.md)

Air Paste is a Rust-based shared clipboard for Windows and macOS: copy text or files on device A, press a hotkey on device B, and it pastes there. Text is end-to-end encrypted; files transfer peer-to-peer on the LAN (with an encrypted server relay as fallback), and the server never stores file contents.

## How it works in daily use

Run the tray app (`airpaste-tray`, menu bar on macOS / system tray on Windows) on every device. Then:

- **Send**: copy something normally (`Cmd+C` / `Ctrl+C`), then press **`Option+C`** (macOS) / **`Alt+C`** (Windows) to publish it to your other devices.
- **Receive**: focus the target app and press **`Option+V`** / **`Alt+V`** — the latest text is pasted, or the latest pending files are downloaded and pasted.
- No hotkeys needed if you prefer the window: type text and click **Send**, drag files into the window to send, click **Copy**/**Download** on inbox items to receive.

This describes **isolated mode** — the tray app's default and our recommendation; see the next section.

## Clipboard modes: isolated vs. system (we recommend isolated)

The agent has two clipboard modes, controlled by the **"Isolated mode"** checkbox in the tray window or `--clipboard-mode=isolated|system` on the CLI. **The tray GUI defaults to isolated; the CLI agent defaults to system.**

### Isolated mode (recommended)

AirPaste is fully decoupled from the system clipboard; both sending and receiving are explicit:

- **Sending is explicit**: copy normally (`Cmd+C` / `Ctrl+C`), then press `Option+C` / `Alt+C` — only then is the current clipboard end-to-end encrypted and published. Sending the same content twice is deduplicated, so double-pressing publishes once and stale clipboard content is never re-sent by accident.
- **Receiving is explicit**: remote text lands only in the AirPaste **inbox** (viewable in the window, with a Copy button) and **never** overwrites your system clipboard on its own. Pressing `Option+V` / `Alt+V` pastes the latest text into the focused app — the system clipboard is borrowed for the paste and restored afterwards. If the latest arrival is a file, the hotkey downloads and pastes it instead.

Why we recommend it:

- **Nothing leaks**: not every copy leaves your machine — only what you explicitly send with `Option+C` / `Alt+C`. System mode relies on heuristic sensitive-content filtering as a safety net; isolated mode doesn't need it by design.
- **Nothing is clobbered**: a copy on another device can never silently overwrite what you were about to paste locally.

### System mode

The classic auto-syncing clipboard:

- every local text copy is published automatically (after sensitive-content filtering: private keys, JWTs, tokens, OTP-like and card-like numbers are skipped);
- remote text is written straight into your system clipboard, ready for a plain `Cmd+V` / `Ctrl+V`.

More convenient, at a cost: every copy is sent out by default (the filter is only heuristic), and remote content can overwrite your local clipboard without warning. Best suited for two devices that are both yours, on a trusted LAN.

**Files** behave the same in both modes: an incoming file manifest is only recorded as pending until you press `Option+V` / `Alt+V` (or click **Download** in the inbox).

To switch: toggle the "Isolated mode" checkbox in the tray window, or start the CLI with `--clipboard-mode=isolated` / `--clipboard-mode=system`.

> macOS permission note: `Option+V` (simulating a paste into other apps) requires Accessibility permission; `Option+C` only reads the clipboard and needs no permission.

## Quick start (GUI only, no command line)

You need two devices, A and B. A also acts as the server.

Build and launch the tray app on each device (no official installers yet; on Windows you can package a portable build for other machines, see "Building" below):

```bash
# macOS
scripts/bundle-macos.sh && open dist/AirPaste.app
```

```powershell
# Windows: pull + build + start the tray in one go (set up the toolchain first, see "Building" below)
powershell -ExecutionPolicy Bypass -File .\scripts\update-build-run-windows.ps1
```

Then, all in the tray window:

1. **On A**: open the settings panel, keep the default server URL `http://127.0.0.1:14444`, check **“Run server on this device”**, click **Save & Connect**. As the first device it is trusted automatically → `● Connected`.
2. **On A**: click **“Generate pair code”** and note the 6-digit code.
3. **On B**: enter A's LAN address (`http://<A's IP>:14444`) as the server URL, paste the pair code, click **Save & Connect** → `● Connected`.
4. Copy on one device, press `Option+C` / `Alt+C` to send; press `Option+V` / `Alt+V` on the other to paste. Done.

> macOS: grant Accessibility permission (System Settings → Privacy & Security → Accessibility) to the tray app, otherwise the paste hotkey cannot type into other apps. Check **“Launch at login”** on each device if you want it to start automatically.

The full walkthrough — CLI usage, pairing details, auth tokens, troubleshooting — is in [docs/USER_MANUAL.md](docs/USER_MANUAL.md) (Chinese).

## iPhone via Shortcuts (no app required)

An iPhone joins through two Shortcuts that call the server's simple-device text endpoints (`/v1/simple/*`):

1. Enable a simple-device token on the server: fill in "简单设备令牌" in the tray settings, or pass `--simple-token <secret>` to the CLI server. The token only opens the simple text endpoints — never device listing, encrypted clips, files, or relays.
2. Enable mirroring on the desktop ("镜像给简单设备" in the tray, or `--simple-mirror=true`): text you explicitly send with `Option+C` / `Alt+C` is additionally mirrored as plaintext into server memory for the iPhone to fetch; auto-published clipboard changes are never mirrored.
3. Create two Shortcuts on the iPhone: "Send clipboard" (Get Clipboard → POST `/v1/simple/clips`) and "Receive clipboard" (GET `/v1/simple/clips/latest` → Copy to Clipboard). Bound to Back Tap, they feel like a phone-side `Option+C` / `Option+V`.

Note: this channel is plaintext to the server (E2E encryption degrades to transport encryption), which is why only explicit sends are mirrored, and the plaintext lives in server memory for at most 10 minutes. Use an HTTPS reverse proxy outside trusted LANs. Full setup guide: [docs/IOS_SHORTCUTS.md](docs/IOS_SHORTCUTS.md) (Chinese).

## Building

macOS:

```bash
cargo build -p airpaste-tray            # tray GUI (embedded agent)
cargo build -p airpaste-server -p airpaste-agent   # CLI server + agent
```

Windows (first time, installs Rust + a portable WinLibs MinGW toolchain under `tools/winlibs`; omit `-Proxy` if direct network access works):

```powershell
.\scripts\setup-windows-toolchain.ps1 -Proxy "http://127.0.0.1:7897"
$env:PATH = "$(Get-Location)\tools\winlibs\mingw64\bin;$env:PATH"
cargo +stable-x86_64-pc-windows-gnu build -p airpaste-tray -p airpaste-server -p airpaste-agent
```

Windows day-to-day update and packaging scripts:

```powershell
# Pull latest code + build the whole workspace + restart the tray in one go (add -Release for a release build)
powershell -ExecutionPolicy Bypass -File .\scripts\update-build-run-windows.ps1

# Package a Windows portable build: release-builds the tray and produces
# dist\AirPaste-portable-<commit>-windows\ (AirPaste.exe + README.txt) plus a matching zip.
# Add -IncludeCli to bundle airpaste-agent.exe / airpaste-server.exe as well.
powershell -ExecutionPolicy Bypass -File .\scripts\package-windows-portable.ps1
```

The portable zip can be copied to any other Windows machine and run after unzipping — no Rust required there. Settings and device identity live under `%APPDATA%\AirPaste`; to upgrade, quit the tray and replace the folder with a newer build.

> Note: both scripts default to the MSVC toolchain (`1.88.0-x86_64-pc-windows-msvc`). If you use the GNU/WinLibs toolchain from above, pass `-Toolchain stable-x86_64-pc-windows-gnu`; `-Proxy` is supported as well.

## Design goals

- server as control plane;
- LAN/direct peer transfer as the preferred data plane;
- end-to-end encrypted stream relay as the reliability fallback;
- no default server-side file storage;
- encrypted text history as an opt-in convenience;
- explicit remote paste hotkey for reliable MVP file paste.

Documentation:

- [docs/USER_MANUAL.md](docs/USER_MANUAL.md) — full user manual (Chinese)
- [docs/DESIGN.md](docs/DESIGN.md)
- [docs/SESSION_HANDOFF.md](docs/SESSION_HANDOFF.md)
- [docs/MACOS_AGENT_PLAN.md](docs/MACOS_AGENT_PLAN.md)

## Workspace layout

- `crates/airpaste-core`: shared domain types.
- `crates/airpaste-protocol`: REST and WebSocket DTOs.
- `crates/airpaste-crypto`: end-to-end content encryption (X25519 + XChaCha20-Poly1305).
- `crates/airpaste-server`: Axum server with embedded `redb` storage.
- `crates/airpaste-agent`: CLI agent for text sync and file manifest publishing.
- `crates/airpaste-tray`: cross-platform tray GUI embedding the agent.

## CLI server and agent (advanced / headless)

The tray app can host the server for you; run these directly only for headless or scripted setups.

Run the server:

```powershell
cargo +stable-x86_64-pc-windows-gnu run -p airpaste-server -- --bind 0.0.0.0:14444 --db .\airpaste.redb
```

For a DDNS/private deployment, start the server with `--auth-token <secret>` or `AIRPASTE_AUTH_TOKEN=<secret>`. Health checks stay public; all other REST and WebSocket APIs require `Authorization: Bearer <secret>`. Agents use the same value with `--auth-token <secret>`.

Sensitive server APIs also require the request device to be trusted and to prove possession of its Ed25519 private key. Agents sign REST and WebSocket requests with `x-airpaste-device-id`, `x-airpaste-signature-alg`, `x-airpaste-timestamp`, `x-airpaste-nonce`, `x-airpaste-body-sha256`, and `x-airpaste-signature`. The first registered device in a fresh database is trusted for bootstrap; later devices must be paired before they can list devices, create/read clips, open WebSocket sync, or create relay sessions. Device registration and pair confirmation remain available to untrusted devices.

Useful endpoints:

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

Run the agent against a local server:

```powershell
.\target\debug\airpaste-agent.exe --server-url http://127.0.0.1:14444 --state-path .\.airpaste-agent-a.json --device-name "PC A" --auth-token "<secret-if-server-enabled-it>"
```

To join a non-first device, create a pairing code through `POST /v1/pair/start` from an already trusted device, then start the new agent with `--pair-code <code>`. Alternatively, an already trusted device can approve a registered device directly — the trust button next to an untrusted device in the tray's devices tab, or `--trust-device <device-id>` from the CLI. The first registered device in a fresh database is trusted automatically for bootstrap.

Current agent scope:

- Text clips are end-to-end encrypted. The agent generates an X25519 key alongside its Ed25519 identity, registers the public key, and seals each clip's content with a random per-clip key wrapped for every trusted device. The server only stores ciphertext, ephemeral public keys, and nonces. Legacy plaintext clips are still applied on read with a warning. Devices registered before this feature re-register automatically to advertise their encryption key.
- Windows text clipboard publish/apply.
- Windows file clipboard manifest publish via `CF_HDROP`.
- MVP file payload download from source agent peer HTTP service into receiver cache.
- Downloaded files are written back to the system clipboard as a file drop list.
- Remote paste hotkey: `Alt+V` (macOS: `Option+V`). In isolated clipboard mode, `Alt+C` / `Option+C` publishes the current clipboard.
- Text clips published by the agent default to a 600-second server-side TTL. Use `--text-clip-ttl-secs 0` to disable text expiry for debugging.
- Automatic text clipboard publishing skips obvious sensitive content by default, including private keys, JWTs, bearer tokens, provider tokens (`ghp_`, `github_pat_`, `sk-`), secret-like assignments, one-time-code-like numbers, credit-card-like numbers, and text above `--max-text-clip-bytes`. Use `--filter-sensitive-text=false` for debugging.

File transfer MVP notes:

- The source agent exposes `GET /v1/files/{transfer_token}/{index}` on its `--peer-bind` address, which defaults to `0.0.0.0:17390` so peers on the LAN can reach it.
- Agents discover each other on the LAN over mDNS (`_airpaste._tcp.local.`, `device_id` in TXT). The receiver prefers a discovered peer's address over the manifest, so `--peer-public-url` is usually unnecessary on a LAN. The file manifest still includes `source_peer_url` as a fallback when mDNS is unavailable; set `--peer-public-url` for that case.
- When direct/LAN transfer is not possible, start the receiver with `--prefer-relay` to pull files through the server-mediated encrypted relay. Both devices connect outbound to `GET /v1/relay/{session_id}/ws`; the source seals each file end-to-end for the recipient (X25519 + XChaCha20-Poly1305) before it traverses the server, which only forwards opaque frames and never sees plaintext. The relay reuses the same signed peer-file authorization as the direct path.
- Peer file requests must include `x-airpaste-clip-id`, `x-airpaste-source-device-id`, `x-airpaste-requester-device-id`, and an Ed25519 `x-airpaste-signature`; the source agent verifies the requester against trusted device public keys from the server.
- The peer transfer token has a local TTL, defaults to 600 seconds, and each file index can be downloaded once.
- File manifest publication is limited by `--max-file-count`, `--max-single-file-bytes`, and `--max-total-file-bytes`.
- New file manifests include lowercase hex SHA-256 for regular files.
- Receivers reject remote file manifests whose regular files exceed `--max-single-file-bytes`, stream peer downloads into temporary files, and verify downloaded byte counts plus SHA-256 before writing files into the cache. Older manifests without SHA-256 fall back to size-only verification with a warning.
- Only regular files are downloaded in this MVP. Directories are announced in the manifest but skipped by transfer.
- Downloaded files are written under `--cache-dir/<transfer_token>/`.
- By default, a remote file manifest is only recorded as pending. Press `Alt+V` (macOS: `Option+V`) on the receiver to download the latest pending files, write them to the local clipboard, and send a normal paste.
- `--auto-apply-files=true` downloads remote files as soon as the manifest arrives. This is mainly useful for smoke tests and debugging.
- `--apply-latest-files-once` downloads the latest remote file clip once, writes the downloaded file references to the local clipboard, prints the downloaded paths as JSON, and exits. This is useful for macOS hotkey/pasteboard debugging.
- `--auto-paste-files=true` sends `Ctrl+V` to the current foreground app after an automatic file apply, so keep it disabled unless the receiver is intentionally focused on the target app.

## Smoke tests

```powershell
.\scripts\smoke-agent.ps1
```

On macOS:

```bash
scripts/smoke-agent-macos.sh
scripts/smoke-agent-macos.sh --auth-token airpaste-smoke-secret
scripts/smoke-hotkey-macos.sh
```

`smoke-hotkey-macos.sh` is interactive: it prepares a pending file clip, then waits for you to press the remote paste hotkey (`Option+V`). Note: the script itself still references the old `Ctrl+Shift+V` chord and needs updating before use.
