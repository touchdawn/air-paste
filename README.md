# Air Paste

Air Paste is a Rust-based shared clipboard for Windows and macOS.

The design goal is:

- server as control plane;
- LAN/direct peer transfer as the preferred data plane;
- no default server-side file storage;
- encrypted text history as an optional convenience;
- explicit remote paste hotkey for reliable MVP file paste.

Start here:

- [docs/DESIGN.md](docs/DESIGN.md)

Recommended first implementation step:

1. Create the Rust workspace layout described in `docs/DESIGN.md`.
2. Implement shared protocol/domain types.
3. Build the server health check and WebSocket skeleton.
4. Build the agent config and startup skeleton.

