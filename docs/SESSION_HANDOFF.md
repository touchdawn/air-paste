# Air Paste Handoff

Last updated: 2026-06-07

This document is for the next coding session. It summarizes the current repo state, what is already working, what is intentionally still MVP-grade, and the recommended next steps.

## Current Repository State

Branch: `main`

Recent commits:

- `3b742f5 test: assert sha256 in macos smoke`
- `ef66401 Merge remote-tracking branch 'origin/main' into codex/macos-agent`
- `6350a59 feat: improve macos agent smoke and hotkey`
- `711f77d feat: verify peer files with sha256`
- `c2de004 feat: enforce single file transfer limits`
- `16ac9af feat: verify peer file download sizes`
- `81d693f feat: stream peer file downloads`
- `935edaf Merge remote-tracking branch 'origin/main' into codex/macos-agent`
- `fda0af2 feat: add macos agent clipboard and hotkey`

The workspace currently contains:

- `crates/airpaste-core`: shared IDs and domain models.
- `crates/airpaste-protocol`: REST and WebSocket DTOs.
- `crates/airpaste-crypto`: end-to-end content encryption (X25519 key agreement + XChaCha20-Poly1305 AEAD, per-clip content key wrapped per recipient).
- `crates/airpaste-server`: Axum control-plane server using `redb`.
- `crates/airpaste-agent`: Windows/macOS agent MVP with end-to-end encrypted text sync, file manifest, peer file server, file download, remote paste hotkeys, Ed25519 device identity, and X25519 encryption identity.

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
cargo +stable-x86_64-pc-windows-gnu build -p airpaste-agent -p airpaste-server
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-agent.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-agent.ps1 -Bind 127.0.0.1:18082 -AuthToken airpaste-smoke-secret
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-server.ps1
```

On macOS:

```bash
cargo check
cargo test
scripts/smoke-agent-macos.sh
scripts/smoke-agent-macos.sh --auth-token airpaste-smoke-secret
scripts/smoke-hotkey-macos.sh
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
- Relay session metadata exists, but the relay data path is not implemented (the cross-network fallback when LAN discovery does not apply).

Platform:

- Windows supports clipboard text, file drop lists, `Ctrl+Shift+V`, and synthetic paste.
- macOS supports clipboard text, file URL read/write, `Ctrl+Shift+V`, and one-shot file apply. Synthetic paste is not implemented yet.
- No tray UI, settings UI, installer, or service/login-item packaging yet.

## Recommended Next Steps

### 1. Extend Encryption Beyond Text

Text clips are now end-to-end encrypted. Remaining gaps:

- Encrypt file manifests and (later) image payloads with the same `airpaste-crypto` primitives.
- Bind clip content to the source device (AEAD AAD or a signature over the ciphertext) so recipients can verify authorship, not just confidentiality.
- Add a UI/CLI fingerprint comparison for device public keys before trusting them.
- Consider hiding plaintext length (currently leaked via `TextClip.utf8_len`).

### 2. Improve File Data Plane

Useful incremental improvements:

- Add directory walking with file count and total-size caps.
- Reuse the per-clip content-key + X25519 wrapping design for peer file payloads.

### 3. Continue macOS Agent

See `docs/MACOS_AGENT_PLAN.md`.

Useful next macOS steps:

- Manually verify `Ctrl+Shift+V` against Finder and common target apps.
- Use `scripts/smoke-hotkey-macos.sh` as the first manual hotkey check; it prepares a pending file clip and waits for a real `Ctrl+Shift+V`.
- Add paste simulation with clear Accessibility permission handling.
- Decide whether to replace or augment `arboard` with lower-level `NSPasteboard` glue if file URL behavior is not reliable enough.
- Add LaunchAgent/login item packaging later, after CLI behavior is stable.
