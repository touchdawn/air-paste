# Air Paste Handoff

Last updated: 2026-06-08

This document is for the next coding session. It summarizes the current repo state, what is already working, what is intentionally still MVP-grade, and the recommended next steps.

## Current Repository State

Branch: `main` (remote `origin` = Gitee `gitee.com:touch_dawn/air-paste`). Development is now
primary on macOS; commit directly to `main`. A Windows machine is used to compile/run the
Windows agent (reached over Remote Desktop). See "Toolchain Notes" for cross-compiling the
Windows target from macOS.

### What changed in the most recent session (2026-06-07)

Building on the existing Windows/macOS MVP (text sync, file manifest + signed peer download
with SHA-256, device identity, pairing/trust, server control plane), this session added, in
order:

1. Provider-token detection in the sensitive-text filter (`ghp_`/`github_pat_`/`sk-`).
2. macOS->Windows cross-compile workflow (`scripts/cross-windows.sh`).
3. **End-to-end encryption of text clips** (new `airpaste-crypto` crate; server sees only
   ciphertext).
4. First **real Mac<->Windows LAN validation** (text decrypt + file transfer; see that section).
5. **mDNS LAN discovery** (`_airpaste._tcp.local.`) so receivers auto-resolve the source's
   address; default `--peer-bind` is now `0.0.0.0:17390`.
6. **Encrypted relay data path** (`GET /v1/relay/{session_id}/ws`) with automatic
   direct->relay fallback, plus network-loss hardening (reconnect backoff, connect/receive
   timeouts) and resilient clipboard polling.
7. **Relay/fallback hardening** (commit `593bb03`):
   - Source file grants are now **commit-on-complete**: an index is only consumed against
     the one-time grant after its bytes finish streaming; a failed/aborted transfer
     **releases** it for retry. The direct HTTP path uses a streaming drop-guard
     (`GrantStream`); the relay path commits/releases explicitly.
   - The direct->relay fallback is now **partial**: already-downloaded indexes are threaded
     through, so the relay retry pulls only the missing files instead of re-pulling the whole
     transfer (which previously hit `410 already served`).
   - The server relay now uses **bounded, backpressured** per-direction queues (split
     read/write tasks, no deadlock, no frame drops) and **enforces the session TTL
     mid-connection**, not just at connect.
8. **Isolated clipboard mode** (commit `593bb03`): a new
   `--clipboard-mode isolated` keeps the AirPaste text channel separate from the system
   clipboard. Remote text lands in an in-app inbox (the system clipboard is never
   auto-overwritten); `Ctrl+Shift+C` captures the current selection into AirPaste and
   `Ctrl+Shift+V` pastes the inbox text into the focused app, both via synthetic copy/paste
   with a save/restore dance so the system clipboard is left untouched. This adds the first
   **macOS synthetic copy/paste** (CoreGraphics `CGEvent`, requires Accessibility permission;
   `crates/airpaste-agent/src/paste/macos.rs`). Text-only for now; files keep the existing
   flow. See "Isolated Clipboard Mode" below.

### What changed in the 2026-06-08 session

Committed the previous session's work, validated it on the real Windows agent, and built a
macOS menu-bar UI:

9. **Windows validation + fixes**: ran the agent build/tests/smokes on the real Windows GNU
   toolchain. Fixed `smoke-agent.ps1` (it compared `encrypted_inline_body` to plaintext —
   stale since text E2EE); switched agent logging to **stderr** (block-buffered stdout never
   flushed for a long-running agent redirected to a file on Windows); pinned `RUST_LOG` in the
   log-grepping smokes. Added Windows smokes `smoke-isolated.ps1` (inbound isolation) and the
   interactive `smoke-isolated-hotkey.ps1`. Confirmed on Windows: build, unit tests,
   system-mode sync, isolated inbound, and `Ctrl+Shift+V/C`. (RDP `rdpclip` adds seconds of
   clipboard latency — an environment limitation; see "Windows / RDP validation".)
10. **macOS menu-bar UI** (`crates/airpaste-tray`, egui/eframe + tray-icon): extracted the
    agent into the `airpaste_agent` library (`spawn_embedded` + `AgentHandle`) and built a
    menu-bar app that embeds it — Chinese UI (CJK font), menu-bar-only (accessory app,
    close-to-hide), live status + inbox + "copy", and a runtime isolated-mode toggle. Verified
    end-to-end on macOS (connects, isolated inbox populates). See "Menu-bar UI".
11. **Windows tray UI** (`crates/airpaste-tray` is now cross-platform): first verified that
    eframe's default glow/OpenGL backend + tray-icon cross-compile to `x86_64-pc-windows-gnu`
    (cargo check AND a full mingw-w64 link build on macOS — no wgpu fallback needed). Then split
    the crate into a shared egui `App`/`run()` (`app.rs`) and per-OS bits (`platform.rs`): CJK
    font path (`C:\Windows\Fonts\msyh.ttc` Microsoft YaHei on Windows) and the tray-only window
    policy (macOS Accessory activation; Windows `with_taskbar(false)` → winit skip-taskbar).
    Added `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` so release builds
    are truly tray-only. **Verified on the real Windows GNU host** (release build): links cleanly
    on the WinLibs toolchain, the window renders Chinese (微软雅黑), no console window, no taskbar
    button, and the tray icon is present. **End-to-end connection also verified** (new
    `scripts/smoke-tray-connect.ps1`, tray as receiver): paired + ● 已连接, device id shown, a
    published text clip decrypted into the isolated inbox and 复制到剪贴板 worked. Remaining: the
    tray right-click menu (显示/退出) was not reliably click-tested (test-harness click-targeting
    issue, not a code one — the menu code is shared with the verified macOS path). See "Menu-bar UI".

Recent commits (newest first): `83fd0f2` Windows tray: hide console in release,
`3703107` cross-platform tray (macOS + Windows); `fe3eaf2` tray UI docs, `e766fd3` runtime isolated toggle,
`33cdc5d` menu-bar-only, `abdc979` CJK/Chinese UI, `a833880` tray↔agent wiring, `46788ce`
agent→library refactor, `d85bfc0` tray scaffold; `0d3fd31` Windows/RDP validation notes,
`5944f53` Windows hotkey harness, `a382b28` pin RUST_LOG, `6e16882` log to stderr, `7d7bf5f`
Windows isolated smoke, `0bf080d` fix Windows smoke for E2EE; `593bb03` relay/fallback
hardening + isolated clipboard mode; `4f1feb6` prior handoff.

The workspace currently contains:

- `crates/airpaste-core`: shared IDs and domain models.
- `crates/airpaste-protocol`: REST and WebSocket DTOs.
- `crates/airpaste-crypto`: end-to-end content encryption (X25519 key agreement + XChaCha20-Poly1305 AEAD, per-clip content key wrapped per recipient).
- `crates/airpaste-server`: Axum control-plane server using `redb`.
- `crates/airpaste-agent`: lib + thin bin. The agent MVP (E2EE text sync, file manifest, peer file server, file download, remote paste hotkeys, Ed25519/X25519 identities) lives in the `airpaste_agent` library; `run_cli()` is the CLI entry and `spawn_embedded(args) -> AgentHandle` lets the tray run it in-process.
- `crates/airpaste-tray`: cross-platform menu-bar / tray UI (egui/eframe + tray-icon) that embeds the agent. A shared egui `App` + `run()` (`app.rs`) plus per-OS bits (`platform.rs`: CJK font path, tray-only window policy). Builds on macOS and Windows; cross-compiles to `x86_64-pc-windows-gnu` (eframe's default glow/OpenGL backend links under mingw-w64 — no wgpu needed).

## Toolchain Notes

Development is now primary on macOS. The server and the macOS agent build, test, and
run natively here. Windows-target code is compile-checked from macOS via cross-compile;
running/behavior-testing the Windows agent (clipboard, hotkey, synthetic paste) still
requires a real Windows session.

### macOS Cross-Compile To Windows

One-time setup:

```bash
rustup target add x86_64-pc-windows-gnu
brew install mingw-w64
```

Then compile-check or build the Windows target from macOS:

```bash
scripts/cross-windows.sh            # cargo check (fast)
scripts/cross-windows.sh build      # full build + link -> target/x86_64-pc-windows-gnu/debug/*.exe
```

The script sets `CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc` so it
overrides the repo `.cargo/config.toml` (which hardcodes a Windows-only `.exe` linker path
the Windows host needs). The repo config is intentionally left untouched.

### Windows Host (compile/run only)

This Windows machine does not have the MSVC linker installed. Use the GNU Rust toolchain plus the portable WinLibs toolchain under `tools/winlibs`.

Use this PATH before Cargo commands:

```powershell
$env:PATH = "D:\ep\air-paste\tools\winlibs\mingw64\bin;$env:PATH"
```

Main commands:

```powershell
cargo +stable-x86_64-pc-windows-gnu check
cargo +stable-x86_64-pc-windows-gnu build -p airpaste-agent -p airpaste-server -p airpaste-tray
cargo +stable-x86_64-pc-windows-gnu build --release -p airpaste-tray   # tray-only (no console/taskbar)
cargo +stable-x86_64-pc-windows-gnu test -p airpaste-agent
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-agent.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-agent.ps1 -Bind 127.0.0.1:18082 -AuthToken airpaste-smoke-secret
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-server.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-isolated.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-isolated-hotkey.ps1   # interactive; needs a desktop session
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-tray-connect.ps1      # visual: tray UI connects + inbox populates (needs a desktop session)
powershell -ExecutionPolicy Bypass -File .\scripts\install-windows.ps1        # copy release exe to %LOCALAPPDATA%\AirPaste (stable path for autostart)
```

On macOS:

```bash
cargo check
cargo test
scripts/smoke-agent-macos.sh
scripts/smoke-agent-macos.sh --auth-token airpaste-smoke-secret
scripts/smoke-relay-macos.sh
scripts/smoke-relay-macos.sh --auth-token airpaste-relay-secret
scripts/smoke-isolated-macos.sh
scripts/smoke-isolated-macos.sh --auth-token airpaste-iso-secret
scripts/smoke-hotkey-macos.sh
scripts/bundle-macos.sh                 # build dist/AirPaste.app (menu-bar accessory)
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
- `GET /v1/relay/{session_id}/ws` (relay data pipe; source and recipient connect, server forwards opaque frames)
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
- Clip `GET`/latest/history queries ignore expired clips. The store prunes expired clips opportunistically on clip creation and read paths.

## Current Desktop Agent Behavior

Agent state file now stores:

- `device_id`
- `device_private_key` (Ed25519 signing key)
- `device_encryption_private_key` (X25519 key-agreement key)

The agent:

- Generates an Ed25519 signing identity and an X25519 encryption identity on first run.
- Registers both the signing public key and the encryption public key with the server.
- Reuses the saved private keys on later runs. A device that predates encryption keys re-registers automatically (state `device_id` is cleared) so the server learns its encryption public key.
- Can confirm pairing with `--pair-code <code>`.
- Publishes text clips end-to-end encrypted: a random per-clip content key encrypts the body (XChaCha20-Poly1305), and the content key is wrapped per trusted device via X25519. Applies remote text clips by unwrapping with its own X25519 key. Legacy plaintext clips are still applied with a warning.
- Gives text clips a default 600-second `expires_at`; use `--text-clip-ttl-secs 0` to publish non-expiring text clips for debugging.
- Skips automatic text clipboard publish for obvious sensitive content by default: private keys, JWTs, bearer tokens, provider tokens (`ghp_`, `github_pat_`, `sk-`), secret-like assignments, one-time-code-like numbers, credit-card-like numbers, and text above `--max-text-clip-bytes`.
- Publishes file clipboard manifests from Windows `CF_HDROP` and macOS file URLs.
- Runs a peer HTTP server on `--peer-bind`, default `0.0.0.0:17390` (LAN-reachable; protected by signed one-time-token requests).
- Advertises and browses `_airpaste._tcp.local.` over mDNS, keeping a `device_id -> LAN address` directory. On download it prefers the discovered address over the manifest `source_peer_url`, so `--peer-public-url` is usually unnecessary on a LAN. mDNS failures fall back to `source_peer_url`.
- Receives remote file manifests and records them as pending by default.
- Downloads pending files on `Ctrl+Shift+V`, writes downloaded cache paths to Windows file clipboard, then sends normal paste.
- On macOS, downloads pending files on `Ctrl+Shift+V` and writes downloaded cache file URLs to the pasteboard. Synthetic `Cmd+V` paste is still intentionally out of scope.
- Supports `--apply-latest-files-once` to fetch the latest remote file clip, download its files, write them to the local clipboard/pasteboard, print downloaded paths as JSON, and exit.
- Pulls remote files through the server-mediated encrypted relay, either automatically when a direct/LAN download fails or always when started with `--prefer-relay true`. The recipient creates a relay session; the server pushes `TransferRelayReady` to both devices; both connect to `GET /v1/relay/{session_id}/ws`; the source serves files claimed from its `PeerFileRegistry` (same signed peer-file authorization as the direct path), sealing each file for the recipient with `airpaste-crypto::seal_bytes`; the server only forwards opaque frames. Direct/LAN transfer is still tried first by default.
- Fallback is incremental: files already downloaded over the direct path are not re-pulled over the relay (the relay only requests still-missing indexes), and source grants are committed only after a file finishes streaming, so a transfer that fails partway can be completed over the relay instead of failing with `410 already served`.
- `poll_clipboard` logs and skips transient local clipboard read failures instead of exiting, so a momentary OS clipboard error does not kill the agent.
- Reconnects the control websocket with exponential backoff (2s up to 30s) and a 10s connect timeout, so a network drop or change does not busy-reconnect or hang. Relay connects also time out (10s); relay receives time out (recipient 30s, source idle 60s).
- Uses macOS defaults `~/Library/Application Support/AirPaste/agent.json` for state and `~/Library/Caches/AirPaste` for cache when paths are not explicitly provided.

Useful agent flags:

- `--server-url`
- `--auth-token`
- `--pair-code`
- `--create-pair-code`
- `--print-latest-clip`
- `--publish-text-once`
- `--apply-latest-files-once`
- `--state-path`
- `--peer-bind`
- `--peer-public-url`
- `--cache-dir`
- `--text-clip-ttl-secs`
- `--filter-sensitive-text`
- `--max-text-clip-bytes`
- `--max-file-count`
- `--max-single-file-bytes`
- `--max-total-file-bytes`
- `--transfer-token-ttl-secs`
- `--auto-apply-files=true`
- `--auto-paste-files=true`
- `--remote-paste-hotkey=false`
- `--publish-clipboard=false`
- `--apply-remote=false`
- `--prefer-relay true` (pull files through the encrypted relay instead of direct/LAN; takes an explicit `true`/`false` value)
- `--clipboard-mode system|isolated` (default `system`; `isolated` enables the in-app inbox + Ctrl+Shift+C / Ctrl+Shift+V text channel)

## Isolated Clipboard Mode

`--clipboard-mode isolated` decouples the AirPaste text channel from the system clipboard
(text only for now; files keep the existing flow). Behaviour vs the default `system` mode:

- Inbound: a remote text clip is decrypted into an in-memory **inbox** (latest one) instead
  of being written to the system clipboard. Log line: `stored remote text in isolated inbox`.
  The system clipboard is never auto-overwritten — this also avoids the RDP `rdpclip`
  clobbering loop.
- Outbound: clipboard polling no longer auto-publishes text. Press **`Ctrl+Shift+C`** to push
  the current selection: the agent synthesizes a copy, reads the selection off the clipboard,
  restores the user's previous clipboard, and publishes the captured text.
- Paste: press **`Ctrl+Shift+V`** to paste the inbox text into the focused app: the agent
  saves the current clipboard, sets it to the inbox text, synthesizes paste, then restores
  the saved clipboard. If the inbox is empty it falls back to the existing file-paste flow.

Implementation:

- `crates/airpaste-agent/src/config.rs` — `ClipboardMode` enum + `--clipboard-mode`.
- `crates/airpaste-agent/src/main.rs` — `ClipboardCtx` (mode + inbox), inbound routing,
  outbound gating, and the `copy_selection_to_airpaste` / `paste_inbox_text` /
  `read_after_copy` orchestration with a save/restore dance (timing consts `CLIPBOARD_SETTLE`,
  `PASTE_CONSUME`, `COPY_POLL_*`).
- `crates/airpaste-agent/src/paste/macos.rs` — **new** macOS synthetic copy/paste via
  CoreGraphics `CGEvent` (Command+C / Command+V), plus `accessibility_trusted`
  (`AXIsProcessTrusted`). Windows reuses `SendInput` (now `copy()` too).
- `crates/airpaste-agent/src/hotkey/*` — second global hotkey (`Ctrl+Shift+C`), registered
  only in isolated mode; the channel now carries a `HotkeyAction` (PasteRemote / CopyToAirPaste).

macOS requirement and caveats:

- Synthetic copy/paste needs **Accessibility permission** (System Settings -> Privacy &
  Security -> Accessibility). The agent logs a warning at startup in isolated mode if the
  process is not trusted. Without it, `Ctrl+Shift+C/V` silently no-op. Note: a CLI binary does
  not appear in the Accessibility list by itself — the grant attaches to the **launching app**
  (the terminal / Claude Code / login item that starts the agent). Granting that app is what
  makes the agent trusted; `AXIsProcessTrusted()` then returns true for the child agent.
- The synthesized chord must wait for the triggering hotkey's Ctrl+Shift to be released first,
  or the held modifiers combine with it (a synthetic Cmd+C arriving while Ctrl is down reads as
  Ctrl+Cmd+C and copies nothing). The copy path sleeps `HOTKEY_MODIFIER_RELEASE` (120ms) before
  `paste.copy()`; the paste path already waits via `CLIPBOARD_SETTLE` after `set_text`. This was
  the one real bug found during real-hardware verification.
- The save/restore timing (`CLIPBOARD_SETTLE` / `PASTE_CONSUME` / `HOTKEY_MODIFIER_RELEASE`) is
  heuristic; if a target app is slow to read the clipboard the restore could race. Tune if needed.

Verified on real macOS hardware (2026-06-07): with Accessibility granted to the launching app,
`Ctrl+Shift+V` pastes the inbox text into TextEdit and leaves the system clipboard unchanged,
and `Ctrl+Shift+C` captures the TextEdit selection (published clip length matched the selection)
and restores the clipboard. The copy direction initially published the stale clipboard until the
`HOTKEY_MODIFIER_RELEASE` delay was added.

Manual test (needs a real GUI session + Accessibility granted; cannot be automated):

1. Run a receiver: `airpaste-agent --server-url ... --pair-code <code> --clipboard-mode isolated`.
   On macOS, grant the binary Accessibility and restart it.
2. Put a sentinel on the local clipboard (copy any text normally).
3. From another paired device, publish text (e.g. copy in isolated mode there, or
   `--publish-text-once "hello from isolated"`). Confirm the receiver logs
   `stored remote text in isolated inbox` and the sentinel is still on the clipboard.
4. Focus a text field, press `Ctrl+Shift+V`. Expect the inbox text to be typed in, and the
   clipboard to still hold the sentinel afterward.
5. Select some text, press `Ctrl+Shift+C`. Expect it to be published (receiver log
   `pushed selection to AirPaste`) and your clipboard unchanged.

On Windows there is a scripted helper for steps 1-5: `scripts/smoke-isolated.ps1` covers the
headless inbound half (inbox stored, clipboard not overwritten), and
`scripts/smoke-isolated-hotkey.ps1` is an interactive harness that sets up the receiver +
sender, opens Notepad, prompts for the two hotkeys, and auto-checks the clipboard restore and
the copy-publish.

### Windows / RDP validation (2026-06-07)

Verified on the real Windows agent at commit `5944f53`:

- `cargo test -p airpaste-agent` (incl. the peer-grant reservation tests) and the existing
  `smoke-agent.ps1` pass; `smoke-isolated.ps1` passes (inbound isolation: a remote text clip
  lands in the inbox without overwriting the system clipboard).
- Interactive `Ctrl+Shift+V` pastes the inbox text into Notepad and restores the system
  clipboard (verified). `Ctrl+Shift+C` fires the handler and the synthetic copy puts the
  selection on the clipboard.

Known RDP limitation (environment, not a code bug): under `rdpclip` (RDP clipboard
redirection), clipboard operations are delayed by several seconds. In one test the
`Ctrl+Shift+V` paste appeared only ~10s later, and `Ctrl+Shift+C` logged
`Ctrl+Shift+C captured no text selection` because the agent's ~600ms read-back of the
synthesized copy raced `rdpclip`'s delayed clipboard rendering. On a local console session
(or with clipboard redirection disabled in mstsc: Local Resources -> uncheck Clipboard) these
are effectively instant. Polling longer is not a real fix for multi-second RDP latency (the
hotkey would just hang), so the code is unchanged; retest the synthetic hotkeys off-RDP.

## Current File Transfer Security

File payloads do not go through the server in the normal MVP path.

Source-side behavior:

- Publishes a `FileClip` manifest to the server.
- Rejects regular files above `--max-single-file-bytes` before publishing a manifest.
- Adds lowercase hex SHA-256 to manifest entries for regular files.
- Registers local original file paths under a short-lived `transfer_token`.
- The token TTL defaults to 600 seconds.
- Each file index can be downloaded once.
- The peer file endpoint is:

```text
GET /v1/files/{transfer_token}/{index}
```

Requester-side behavior:

- Downloads from `source_peer_url`.
- Rejects remote file manifests whose regular files exceed `--max-single-file-bytes`.
- Streams each downloaded file into a temporary cache file.
- Verifies each downloaded file byte count matches `FileEntry.size` and, when present, its SHA-256 matches `FileEntry.sha256` before renaming it into the local cache.
- Falls back to size-only verification for old manifests that omit `FileEntry.sha256`, with a warning.
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
- File manifest and downloaded file SHA-256 verification.
- Local file clipboard write.
- Server auth token path.
- macOS scripted text sync (end-to-end encrypted publish on one device, decrypt + pasteboard apply on a paired device).
- `airpaste-crypto` unit tests: encrypt/decrypt round-trip, persisted-identity decrypt, and that a non-recipient device cannot decrypt.
- macOS scripted file URL publish, signed peer download, file URL pasteboard apply through `--apply-latest-files-once`.
- macOS scripted encrypted-relay file pull (`scripts/smoke-relay-macos.sh`): forces `--prefer-relay true`, asserts the source served over the relay, the recipient downloaded via the relay, and the file is byte-exact (size + SHA-256). Also runs under `--auth-token`.
- Peer file grant reservation unit tests (`airpaste-agent` `peer::tests`): a failed transfer releases the grant for retry, a completed transfer is one-time, and a rejected claim reserves nothing.
- macOS scripted isolated-clipboard inbound test (`scripts/smoke-isolated-macos.sh`): a remote text clip is stored in the isolated inbox WITHOUT overwriting the system clipboard (sentinel survives, no system-clipboard apply path). Also runs under `--auth-token`. The synthetic Ctrl+Shift+C / Ctrl+Shift+V hotkeys are verified manually (see "Isolated Clipboard Mode").
- macOS scripted server auth token path.
- macOS interactive `Ctrl+Shift+V` hotkey harness exists as `scripts/smoke-hotkey-macos.sh`.
- Trusted-device signed API guard path: missing signature returns `401`, untrusted signed request returns `403`, paired signed request is allowed, replayed nonce returns `401`.
- Peer unauthenticated request returns `401`.
- Repeated file index download returns `410`.
- Single-file size limit rejects oversized local file publication.

## Real-Machine Validation (2026-06-07)

First end-to-end test between two real machines on the same LAN (Mac `192.168.50.199`
client, Windows `192.168.50.200`), server on the Mac:

- Text E2EE Mac->Win: decryption verified (`applied remote text clip` logs after AEAD +
  UTF-8 succeed; the server-stored clip is ciphertext only, no plaintext leak).
- File transfer Mac->Win: byte-exact with matching SHA-256, written to the receiver cache
  (`downloaded remote file` + `applied downloaded files`). This path is unaffected by RDP
  clipboard redirection and is the strongest evidence the peer data plane works cross-host.

Known environment artifact (not a code bug): when the Windows box is reached over Remote
Desktop, `rdpclip.exe` bidirectionally mirrors the clipboards, so text written by the agent
to the Windows system clipboard can be clobbered (and `Get-Clipboard` shows stale/empty
content). It also creates a publish feedback loop if the sender runs with
`--publish-clipboard=true`. To verify system-clipboard landing cleanly, disable clipboard
redirection in mstsc (Local Resources -> uncheck Clipboard) and retest.

## Menu-bar UI (`airpaste-tray`)

A cross-platform menu-bar / tray app (egui/eframe window + `tray-icon` menu) that embeds the
agent, running on **both macOS and Windows**. The agent was extracted into the `airpaste_agent`
library: `spawn_embedded(args)` starts it on a background Tokio runtime and returns an
`AgentHandle` the UI polls for connection state, device id/name, clipboard mode, and the latest
isolated-mode inbox text.

The crate is split into a shared egui `App` + `run()` (`src/app.rs`, identical on both OSes)
and the per-OS bits (`src/platform.rs`): the CJK font path (macOS system fonts vs Windows
`C:\Windows\Fonts\msyh.ttc` Microsoft YaHei) and the tray-only window policy (macOS `Accessory`
activation via winit; Windows `ViewportBuilder::with_taskbar(false)`, which egui maps to winit
`skip_taskbar`). `main.rs` is a thin entry that calls `app::run()` on every platform, plus
`#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` to drop the console window
in Windows release builds (a console-subsystem exe otherwise gets its own taskbar button).
`winit` stays a macOS-only direct dependency (only the `Accessory` hook needs it); the GUI deps
(`eframe`, `tray-icon`, `tokio`, `airpaste-agent`) build on all platforms.

Run it (accepts the same flags as the agent):

```bash
cargo run -p airpaste-tray -- --server-url http://<host> --pair-code <code> --clipboard-mode isolated
# or just `cargo run -p airpaste-tray` to use the agent defaults
```

The window (Chinese UI) shows: connection status (● 已连接 / ○ 连接中), device + device id, an
**isolated-mode checkbox** (toggles at runtime), and the latest received text with a "复制到剪贴板"
button. The tray menu has 显示 / 退出.

Implemented:

- **Chinese UI**: a macOS CJK font (Arial Unicode.ttf, fallback Hiragino/STHeiti) is installed
  into egui's `FontDefinitions` (egui's default fonts have no CJK glyphs).
- **Menu-bar-only**: runs with `NSApplicationActivationPolicy.accessory` (via the eframe
  event-loop-builder hook + winit's macOS ext) — no Dock icon, no app menu bar. The window's
  close button hides the window (app stays in the menu bar); only the tray's 退出 exits.
- **Runtime isolated toggle**: isolated mode is a shared `AtomicBool` read live by the agent;
  `AgentHandle::set_isolated` + the checkbox flip it without restarting. The tray defaults to
  isolated (`AIRPASTE_CLIPBOARD_MODE`) so both hotkeys register. Caveat: the inbound/outbound
  text behaviour toggles live, but the `Ctrl+Shift+C` hotkey is only registered if the agent
  *started* isolated (hotkeys cannot be re-registered after launch).

Architecture: eframe owns the platform main-thread event loop; the agent runs on a background
Tokio runtime; the tray polls `AgentHandle` and `MenuEvent` each frame (200ms cadence).

Verified on macOS: the embedded agent registers, upgrades the control WebSocket (101), and
runs isolated mode with the global hotkeys; the font atlas + accessory mode launch without
panics; `cargo check --workspace`, the unit tests, and the three macOS smokes all pass.

Verified on the real Windows GNU host (2026-06-08, release build via the WinLibs toolchain):
`cargo build --release -p airpaste-tray` links cleanly, the egui window renders Chinese in
微软雅黑 (no `no CJK font found` warning, so `C:\Windows\Fonts\msyh.ttc` loaded), there is no
console window and no taskbar button (true tray-only), and the tray icon is present in the
notification-area overflow.

**End-to-end connection verified on Windows** via `scripts/smoke-tray-connect.ps1` (fresh
server + a CLI bootstrap device that mints a pair code; the tray runs as the receiver): the
tray paired (`pairing confirmed trusted=true`), the window flipped to ● 已连接 (green) with the
device id shown, a text clip published from the bootstrap device decrypted into the tray's
isolated inbox (`stored remote text in isolated inbox`, shown under 最近收到), and "复制到剪贴板"
worked. Windows hotkeys (`Ctrl+Shift+V` / `Ctrl+Shift+C`) registered too.

Not yet click-verified on Windows: the tray right-click menu (显示/退出) and close-to-hide — the
menu/close code is shared with the verified macOS path; the miss was a UI-automation
click-targeting issue, not a code one.

UI features added (2026-06-08, after the initial Windows port — commits `06bf51d`, `0dda82b`,
`948ec6a`):

- **Real app icon**: a white paper-plane on a rounded blue tile, drawn in code (`icon_rgba()`),
  used for both the tray icon and the window/taskbar icon. Single swap point for a PNG logo later.
- **Inbox history**: the inbox is a bounded `VecDeque` (latest 20, newest first); the UI shows a
  scrollable list with per-entry copy (`AgentHandle::inbox_history()`).
- **Connection-error display**: `AgentHandle::last_error()` drives a three-state status line —
  ● 已连接 / ✕ 连接失败:<err> / ○ 连接中…. (`AgentShared.last_error` is set when the embedded
  agent stops, e.g. registration fails.)
- **In-window pairing/config**: a "设置 / 连接" panel (server URL / pair code / auth token +
  「保存并连接」) backed by a persisted `TrayConfig` JSON under `airpaste_agent::app_support_dir()`.
  Startup overlays config onto the parsed args only where still default, so CLI/env wins.
  「保存并连接」 persists, clears the cached `device_id` if the server changed, and **re-execs the
  process** so the OS reclaims the agent's bound peer port / hotkeys / mDNS (chosen over an
  in-process restart, which would orphan `run()`'s detached peer/poll/ws tasks). The one-shot pair
  code is cleared from the config once connected (a consumed code is a hard error on re-confirm).
- **Start at login**: a「开机自启」checkbox + `crate::autostart` (macOS LaunchAgent plist;
  Windows `HKCU\…\Run` via `reg.exe`; no extra deps).
- **Packaging**: `scripts/bundle-macos.sh` → `dist/AirPaste.app` (Info.plist `LSUIElement=true`);
  `scripts/install-windows.ps1` → copies the release exe to `%LOCALAPPDATA%\AirPaste`.

Verified on macOS (isolated `HOME`): the tray reads `tray-config.json` with no CLI flags, pairs,
connects, receives a clip into the inbox, and the pair code is cleared from the config afterwards;
the icon artwork was eyeballed via an offline render; `dist/AirPaste.app` builds and launches
without panic. Not yet exercised on real hardware: the interactive config-panel「保存并连接」→
re-exec flow, the「开机自启」toggle (plist / Run-key creation), and (still) the Windows tray
right-click menu (显示/退出). The connection-config UI compiles on both targets.

Not done yet (UI follow-ups): real-hardware pass on the config-panel re-exec + 开机自启 toggle +
Windows right-click menu; a designed PNG/`.icns` logo; richer pairing UX (fingerprint compare).

## Important MVP Limitations

Security and trust:

- Text clips are end-to-end encrypted (X25519 + XChaCha20-Poly1305, per-clip content key wrapped per trusted device); the server only sees ciphertext. File manifests and image clips are NOT encrypted yet.
- The plaintext length of a text clip still leaks via `TextClip.utf8_len`.
- Sender authenticity for clip content relies on the REST request signature, not an AEAD binding to the source device.
- Legacy plaintext text clips are still accepted on read (back-compat) and applied with a warning.
- REST signatures currently use an in-memory nonce cache, so replay protection resets when the server restarts.
- There is no UI fingerprint comparison for device public keys.

Transfer:

- Peer file server streams file responses from disk instead of buffering entire files in memory.
- Directories are represented in the manifest but skipped by transfer.
- There is no recursive directory copy.
- There is no resume, explicit chunk protocol, or transfer progress.
- The relay data path is implemented (E2EE, server-forwarded). The receiver falls back to relay automatically when a direct/LAN download errors, and `--prefer-relay true` forces it. The fallback is now incremental and survives partial direct transfers: only still-missing indexes are pulled over the relay, and a source index is committed against its one-time grant only after the bytes finish streaming (a failed stream releases it), so a mid-transfer direct failure is completed over the relay instead of hitting `410 already served`. Residual edge: if the source finishes pushing an index's bytes but the recipient never receives the tail, that index is committed and a relay retry of *that* index would still see it served (no app-level delivery ACK yet); the partial fallback covers the common connect/mid-stream failures.
- The server relay forwards frames in memory with a **bounded** per-direction queue (`RELAY_QUEUE_CAPACITY` frames, backpressured via split read/write tasks so neither direction deadlocks) and enforces the session byte budget plus the session TTL **mid-connection** (a `tokio::time::sleep` deadline tears the relay down at `expires_at`). The recipient's 30s receive timeout and source's 60s idle timeout still bound shorter stalls.

Platform:

- Windows supports clipboard text, file drop lists, `Ctrl+Shift+V`, and synthetic copy/paste.
- macOS supports clipboard text, file URL read/write, `Ctrl+Shift+V`, one-shot file apply, and now synthetic copy/paste via CoreGraphics `CGEvent` (used by isolated mode; requires Accessibility permission). The file-paste flow on macOS still does not auto-`Cmd+V` after apply (`REMOTE_PASTE_HOTKEY_PASTES_AFTER_APPLY` is false on macOS); only isolated-mode text paste synthesizes the keystroke so far.
- Isolated clipboard mode is text-only; file clips still use the system clipboard / pending-download flow.
- The macOS synthetic copy/paste has been verified on real macOS hardware (Ctrl+Shift+V and Ctrl+Shift+C into TextEdit, system clipboard preserved) with Accessibility granted to the launching app. Windows synthetic copy/paste (SendInput) is compiled but not yet exercised on a real Windows session.
- The tray/menu-bar UI (`airpaste-tray`) runs on macOS and Windows; no settings UI, installer,
  or service/login-item/autostart packaging yet.

## Recommended Next Steps

Direct/LAN + mDNS + encrypted relay (with auto-fallback) now form a working data plane for
text and files. Candidate next steps, roughly prioritized:

### 0. Verify relay on real hardware

The relay was validated only on one macOS host (forced and auto-fallback). It has not yet
run Mac<->Windows on real machines. Worth a quick real-machine check (e.g. on the receiver,
make the source unreachable or pass `--prefer-relay`) before relying on it.

### 1. Harden the relay / fallback — DONE this session (verify on real hardware per #0)

- ~~Make the direct->relay fallback robust to partial direct transfers.~~ Done: grants are
  committed only after the byte stream completes (released on failure), and the fallback pulls
  only still-missing indexes. See `crates/airpaste-agent/src/peer.rs` (`GrantStream`,
  `commit_served`/`release`), `relay.rs`, and `main.rs` (`missing_file_indexes`,
  `apply_file_clip`). Covered by `crates/airpaste-agent/src/peer.rs` unit tests and
  `scripts/smoke-relay-macos.sh`.
- ~~Bound the server relay's in-memory queues and enforce session TTL mid-connection.~~ Done:
  see `crates/airpaste-server/src/relay.rs` (bounded channels + split read/write tasks) and
  `routes.rs` (TTL passed to `relay_ws_handler`).
- Remaining: there is still no app-level delivery ACK, so the one residual case is an index
  whose bytes were fully pushed by the source but never received by the recipient — a relay
  retry of that specific index would see it committed. Add an end-of-file ACK from the
  recipient if this proves to matter in practice.

### 2. Extend encryption beyond text

- Encrypt file manifests and (later) image payloads with the same `airpaste-crypto` primitives
  (`seal_bytes`/`open_bytes` already exist and are used by the relay).
- Bind clip content to the source device (AEAD AAD or a signature over the ciphertext) so
  recipients can verify authorship, not just confidentiality.
- Add a UI/CLI fingerprint comparison for device public keys before trusting them.
- Consider hiding plaintext length (currently leaked via `TextClip.utf8_len`).

### 3. Improve the file data plane

- Add directory walking with file count and total-size caps (directories are currently
  announced in the manifest but skipped by transfer).
- Add transfer progress / resume.

### 3b. Finish isolated clipboard mode

- macOS synthetic `Ctrl+Shift+C` / `Ctrl+Shift+V` verified on real hardware (held-modifier
  leak fixed via `HOTKEY_MODIFIER_RELEASE`). Still TODO: verify the same on a real **Windows**
  session, and confirm the save/restore timing is reliable across more apps (browsers,
  Electron) beyond TextEdit.
- Extend isolated mode to files (currently text-only): the inbox would need to hold the latest
  pending file clip and `Ctrl+Shift+V` choose between text/files by recency.
- Consider a small inbox history (latest N) and a way to pick which entry to paste.

### 3c. Menu-bar UI (`airpaste-tray`)

Done: scaffold + agent wiring, Chinese UI (CJK font), menu-bar-only (accessory, close-to-hide),
runtime isolated-mode toggle, **real app icon, inbox history, connection-error display,
in-window pairing/config (persisted `TrayConfig` + re-exec reconnect), start-at-login toggle, and
lightweight packaging (`bundle-macos.sh` / `install-windows.ps1`)** — see "Menu-bar UI". Next: a
real-hardware pass on the config-panel「保存并连接」→ re-exec and the「开机自启」toggle; a designed
PNG/`.icns` logo; richer pairing UX (device fingerprint compare).

### 3d. Windows UI — DONE (2026-06-08), minor click-test follow-up

The same tray UI now runs on Windows. Confirmed first that the eframe + tray-icon stack
cross-compiles to `x86_64-pc-windows-gnu` from macOS — eframe's **default glow/OpenGL backend
links fine under mingw-w64** (cargo check AND a full link build), so no wgpu fallback was
needed. Then split `crates/airpaste-tray` into a shared egui `App` (`app.rs`) + per-OS bits
(`platform.rs`): CJK font path (`C:\Windows\Fonts\msyh.ttc`) and the tray-only window policy
(macOS `Accessory`; Windows `ViewportBuilder::with_taskbar(false)` → winit skip-taskbar). Added
`#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` so release builds have no
console window/taskbar button. Verified on the real Windows GNU host (release build via WinLibs):
links, renders Chinese in 微软雅黑, no console, no taskbar button, tray icon present, and a full
end-to-end connection (pair → ● 已连接 → isolated inbox populated → copy) via
`scripts/smoke-tray-connect.ps1`. See "Menu-bar UI". Remaining: reliably click-test the tray
right-click menu (显示/退出) and close-to-hide on Windows (the menu code is shared with the
verified macOS path; the miss was a UI-automation targeting issue), then fold Windows UI
follow-ups into 3c.

### 3. Continue macOS Agent

See `docs/MACOS_AGENT_PLAN.md`.

Useful next macOS steps:

- Manually verify `Ctrl+Shift+V` against Finder and common target apps.
- Use `scripts/smoke-hotkey-macos.sh` as the first manual hotkey check; it prepares a pending file clip and waits for a real `Ctrl+Shift+V`.
- Paste simulation now exists for macOS (`paste/macos.rs`, CoreGraphics `CGEvent`) with an Accessibility check; it is used by isolated-mode text paste and is the basis for wiring file-paste auto-`Cmd+V` later. Still needs real-session verification.
- Decide whether to replace or augment `arboard` with lower-level `NSPasteboard` glue if file URL behavior is not reliable enough.
- Add LaunchAgent/login item packaging later, after CLI behavior is stable.
