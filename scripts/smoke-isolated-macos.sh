#!/usr/bin/env bash
set -euo pipefail

# Ensure the agent emits the info-level lines this smoke greps for, regardless of any RUST_LOG
# already in the environment (e.g. RUST_LOG=warn would hide "stored remote text in isolated inbox").
export RUST_LOG="${AIRPASTE_SMOKE_RUST_LOG:-airpaste_agent=info}"

bind="127.0.0.1:18086"
auth_token=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --bind)
      bind="$2"
      shift 2
      ;;
    --auth-token)
      auth_token="$2"
      shift 2
      ;;
    -h|--help)
      cat <<'USAGE'
Usage: scripts/smoke-isolated-macos.sh [--bind 127.0.0.1:18086] [--auth-token TOKEN]

Runs a macOS-only Air Paste isolated-clipboard-mode smoke test (inbound half):
  - a receiver runs with --clipboard-mode isolated
  - the system clipboard is seeded with a sentinel value
  - a remote text clip is published
  - asserts the receiver stored it in its in-app inbox WITHOUT overwriting the system
    clipboard (the sentinel survives, and it does not log a system-clipboard apply)

The synthetic copy/paste hotkeys (Ctrl+Shift+C / Ctrl+Shift+V) need a focused GUI app and
Accessibility permission, so they are verified manually, not here.
USAGE
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if [ "$(uname -s)" != "Darwin" ]; then
  echo "smoke-isolated-macos.sh must run on macOS" >&2
  exit 1
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
server_bin="$root/target/debug/airpaste-server"
agent_bin="$root/target/debug/airpaste-agent"
base_url="http://$bind"
tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/airpaste-isolated-smoke.XXXXXX")"

server_log="$tmpdir/server.log"
receiver_log="$tmpdir/receiver.log"
original_clip="$(pbpaste 2>/dev/null || true)"

server_pid=""
receiver_pid=""
peer_base=$((20000 + RANDOM % 20000))
receiver_peer="127.0.0.1:$peer_base"

stop_pid() {
  local pid="$1"
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
}

cleanup() {
  set +e
  stop_pid "$receiver_pid"
  stop_pid "$server_pid"
  printf "%s" "$original_clip" | pbcopy 2>/dev/null || true
  rm -rf "$tmpdir"
}
trap cleanup EXIT

dump_logs() {
  echo "--- server log ---" >&2
  cat "$server_log" >&2 2>/dev/null || true
  echo "--- receiver log ---" >&2
  cat "$receiver_log" >&2 2>/dev/null || true
}

fail() {
  echo "smoke failed: $*" >&2
  dump_logs
  exit 1
}

parse_pair_code() {
  sed -n 's/.*"code":"\([^"]*\)".*/\1/p'
}

run_agent() {
  if [ -n "$auth_token" ]; then
    "$agent_bin" "$@" --auth-token "$auth_token"
  else
    "$agent_bin" "$@"
  fi
}

create_pair_code() {
  local json code
  json="$(run_agent \
    --server-url "$base_url" \
    --state-path "$tmpdir/bootstrap.json" \
    --device-name "Mac Isolated Bootstrap" \
    --create-pair-code \
    --pair-ttl-seconds 600 \
    --publish-clipboard=false \
    --apply-remote=false \
    --remote-paste-hotkey=false)"
  code="$(printf "%s" "$json" | parse_pair_code)"
  if [ -z "$code" ]; then
    fail "could not parse pair code from $json"
  fi
  printf "%s" "$code"
}

wait_for_health() {
  local deadline=$((SECONDS + 10))
  while [ "$SECONDS" -lt "$deadline" ]; do
    if curl -fsS "$base_url/health" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

wait_for_log() {
  local needle="$1"
  local deadline=$((SECONDS + 10))
  while [ "$SECONDS" -lt "$deadline" ]; do
    if grep -q "$needle" "$receiver_log" 2>/dev/null; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

echo "Building Air Paste binaries"
cargo build -p airpaste-server -p airpaste-agent >/dev/null

server_args=(--bind "$bind" --db "$tmpdir/server.redb")
if [ -n "$auth_token" ]; then
  server_args+=(--auth-token "$auth_token")
fi
"$server_bin" "${server_args[@]}" >"$server_log" 2>&1 &
server_pid=$!
wait_for_health || fail "server did not become healthy at $base_url"

# Seed the system clipboard with a sentinel; isolated mode must NOT overwrite it.
sentinel="airpaste-isolated-sentinel-$(date +%s)"
printf "%s" "$sentinel" | pbcopy
if [ "$(pbpaste)" != "$sentinel" ]; then
  fail "could not seed the system clipboard with the sentinel"
fi

# Pair + start a persistent receiver in isolated mode.
receiver_pair_code="$(create_pair_code)"
run_agent \
  --server-url "$base_url" \
  --state-path "$tmpdir/receiver.json" \
  --device-name "Mac Isolated Receiver" \
  --pair-code "$receiver_pair_code" \
  --peer-bind "$receiver_peer" \
  --cache-dir "$tmpdir/receiver-cache" \
  --clipboard-mode isolated \
  --publish-clipboard=false \
  --remote-paste-hotkey=false \
  >"$receiver_log" 2>&1 &
receiver_pid=$!
wait_for_log "agent started" || fail "isolated receiver did not start"

echo "Publish remote text"
remote_text="airpaste isolated remote $(date +%s)"
run_agent \
  --server-url "$base_url" \
  --state-path "$tmpdir/bootstrap.json" \
  --device-name "Mac Isolated Bootstrap" \
  --publish-text-once "$remote_text" \
  --publish-clipboard=false \
  --apply-remote=false \
  --remote-paste-hotkey=false \
  >/dev/null

wait_for_log "stored remote text in isolated inbox" \
  || fail "receiver did not store remote text in the isolated inbox"

# The core assertion: the system clipboard was NOT overwritten.
current_clip="$(pbpaste 2>/dev/null || true)"
if [ "$current_clip" != "$sentinel" ]; then
  fail "isolated mode overwrote the system clipboard (expected sentinel, got: $current_clip)"
fi

# And it must not have taken the system-clipboard apply path.
if grep -q "applied remote text clip" "$receiver_log"; then
  fail "isolated receiver took the system-clipboard apply path"
fi

stop_pid "$receiver_pid"
receiver_pid=""

echo "Isolated macOS smoke passed"
echo "System clipboard preserved: $sentinel"
