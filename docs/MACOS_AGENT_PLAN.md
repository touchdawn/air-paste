# macOS Agent Development Plan

Last updated: 2026-06-06

This document is for starting Air Paste macOS development in parallel with the Windows/server work.

## Recommendation

Develop the macOS implementation on a real Mac.

Reasons:

- `NSPasteboard`, file URL pasteboard behavior, global hotkeys, CGEvent paste simulation, and Accessibility permissions need real macOS runtime testing.
- Cross-compiling Rust from Windows can compile some shared code, but it cannot reliably validate clipboard, hotkey, paste, Login Item, or permission behavior.
- The current repo already has platform seams in `crates/airpaste-agent/src/clipboard.rs`, `hotkey.rs`, and `paste.rs`; macOS can be added behind those seams without forking the app.

## Current Shared Protocol To Reuse

The macOS agent should use the same:

- `airpaste-core` models.
- `airpaste-protocol` REST/WebSocket DTOs.
- Server auth token flow.
- Ed25519 device identity model.
- Pairing flow with `--pair-code`.
- Peer file request signature format.
- File manifest fields:
  - `files`
  - `total_size`
  - `transfer_token`
  - `source_peer_url`
  - `transfer_expires_at`

The macOS agent must interoperate with the current Windows agent.

## First macOS Milestone

Goal: a command-line macOS agent that can sync text with the existing server and Windows agent.

Scope:

- Build the workspace on macOS.
- Implement macOS text clipboard read/write.
- Reuse current REST/WebSocket code.
- Reuse current device identity and pairing.
- Verify Windows-to-macOS and macOS-to-Windows text sync.

Do not start with UI or packaging.

## Suggested Implementation Shape

Keep the current `airpaste-agent` crate.

Add macOS modules:

```text
crates/airpaste-agent/src/clipboard/macos.rs
crates/airpaste-agent/src/hotkey/macos.rs
crates/airpaste-agent/src/paste/macos.rs
```

Update dispatch modules:

```rust
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::Clipboard;
```

Use `#[cfg(windows)]` and `#[cfg(target_os = "macos")]` explicitly. Keep non-supported platforms returning clear errors.

## Dependencies To Evaluate On macOS

Preferred path:

- Use a mature Rust crate for Objective-C/Cocoa interop only where it keeps code simple.
- Keep low-level platform glue isolated in macOS modules.

Candidate crates:

- `objc2`
- `objc2-foundation`
- `objc2-app-kit`
- `core-foundation`
- `core-graphics`

Validate exact crate names and APIs on the Mac before committing. Do not assume Windows can verify these platform APIs.

## Clipboard: Text

macOS clipboard monitoring is usually polling-based:

- Use `NSPasteboard.generalPasteboard`.
- Track `changeCount`.
- Poll every 500-1000 ms for MVP.
- Read `NSPasteboardTypeString`.
- Write `NSPasteboardTypeString`.

Expected behavior:

- On local text copy, publish a text clip.
- On remote text clip, write string to local pasteboard.
- Avoid feedback loops using the existing `last_local_write` pattern.

## Clipboard: Files

Use file URLs, not raw POSIX path strings, as the primary pasteboard representation.

Read:

- Inspect pasteboard items.
- Extract file URLs from standard file URL pasteboard types.
- Convert file URLs to local paths.

Write:

- Write local downloaded cache file URLs to the pasteboard.
- Preserve filenames and paths under the Air Paste cache directory.

Important:

- Finder and many apps expect file URLs.
- `.app` is a directory bundle and should be marked as `MacAppBundle` later.
- Directory walking should not be implemented until file count and size caps are enforced.

## Remote File Paste Hotkey

Current Windows hotkey is `Ctrl+Shift+V`.

macOS MVP should use:

```text
Cmd+Shift+V
```

Implementation options:

- Use Carbon hotkey APIs for a minimal MVP.
- Or use an event tap/global shortcut crate if it is stable enough.

After a remote file manifest is pending:

1. User presses `Cmd+Shift+V`.
2. Agent downloads files from `source_peer_url`.
3. Agent writes file URLs to `NSPasteboard`.
4. Agent sends a normal paste event, likely `Cmd+V`.

Paste simulation with `CGEvent` may require Accessibility permission.

## Permissions

Expected macOS permission prompts:

- Accessibility permission for synthetic paste events.
- Possibly Automation/Input Monitoring depending on implementation.

For CLI MVP:

- If paste simulation fails, still write files to the pasteboard and log a clear warning.
- Do not require Accessibility permission just to sync text.

## Local Paths

Recommended default cache:

```text
~/Library/Caches/AirPaste
```

Recommended default state:

```text
~/Library/Application Support/AirPaste/agent.json
```

The current agent defaults are relative paths. For macOS, add platform-specific defaults later, or add a config resolver layer.

## Smoke Testing On Mac

Start with a local server:

```bash
cargo run -p airpaste-server -- --bind 127.0.0.1:8080 --db ./airpaste.redb
```

Run first macOS agent:

```bash
cargo run -p airpaste-agent -- \
  --server-url http://127.0.0.1:8080 \
  --state-path ./target/macos-agent-a.json \
  --device-name "Mac A"
```

Pair a second agent/device:

1. Create a pair code with `POST /v1/pair/start`.
2. Run the second agent with `--pair-code <code>`.

Cross-platform smoke target:

- Windows agent publishes text, macOS agent applies it.
- macOS agent publishes text, Windows agent applies it.
- Windows copies a regular file, macOS receives pending manifest.
- macOS remote paste downloads the file and writes file URLs to pasteboard.

## Interop Requirements

The macOS implementation must match current Windows behavior:

- Same server API.
- Same agent state fields.
- Same Ed25519 public/private key encoding.
- Same peer request signing message.
- Same peer headers:
  - `x-airpaste-clip-id`
  - `x-airpaste-source-device-id`
  - `x-airpaste-requester-device-id`
  - `x-airpaste-signature-alg`
  - `x-airpaste-signature`

Do not change the signing message without updating Windows and smoke tests.

## Suggested First PR For macOS

Keep the first macOS PR small:

- Add macOS `Clipboard` implementation for text only.
- Make non-Windows/non-macOS fallback unchanged.
- Add a manual smoke doc for text sync.
- Do not implement file URLs, hotkey, paste simulation, LaunchAgent, or UI yet.

After text works:

1. Add file URL read/write.
2. Add peer file server compatibility.
3. Add `Cmd+Shift+V` hotkey.
4. Add paste simulation and permission handling.
5. Add LaunchAgent/login item.

