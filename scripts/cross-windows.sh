#!/usr/bin/env bash
# Cross-compile the Windows target from macOS.
#
# One-time setup:
#   rustup target add x86_64-pc-windows-gnu
#   brew install mingw-w64
#
# Usage:
#   scripts/cross-windows.sh            # cargo check (fast compile verification)
#   scripts/cross-windows.sh build      # full build + link, produces .exe
#   scripts/cross-windows.sh clippy     # lint the Windows target
#
# This sets the windows-gnu linker via env var so it overrides the repo
# .cargo/config.toml (which hardcodes a Windows-only .exe linker path that
# the Windows compile host needs). The repo config is left untouched.
set -euo pipefail

export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER="${CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER:-x86_64-w64-mingw32-gcc}"

cmd="${1:-check}"
if [ "$#" -gt 0 ]; then
  shift
fi

exec cargo "$cmd" --target x86_64-pc-windows-gnu \
  -p airpaste-agent -p airpaste-server -p airpaste-tray "$@"
