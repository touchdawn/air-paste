#!/usr/bin/env bash
set -euo pipefail

bind="127.0.0.1:18084"
auth_token=""
timeout_secs=60

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
    --timeout-secs)
      timeout_secs="$2"
      shift 2
      ;;
    -h|--help)
      cat <<'USAGE'
Usage: scripts/smoke-hotkey-macos.sh [--bind 127.0.0.1:18084] [--auth-token TOKEN] [--timeout-secs 60]

Prepares a macOS Air Paste pending file clip, waits for you to press Ctrl+Shift+V,
then verifies the receiver downloaded the file and wrote a file URL to the pasteboard.

This script requires a real interactive macOS session. It restores the text clipboard on exit.
USAGE
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

case "$timeout_secs" in
  ''|*[!0-9]*)
    echo "--timeout-secs must be a positive integer" >&2
    exit 2
    ;;
esac
if [ "$timeout_secs" -lt 1 ]; then
  echo "--timeout-secs must be at least 1" >&2
  exit 2
fi

if [ "$(uname -s)" != "Darwin" ]; then
  echo "smoke-hotkey-macos.sh must run on macOS" >&2
  exit 1
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
server_bin="$root/target/debug/airpaste-server"
agent_bin="$root/target/debug/airpaste-agent"
base_url="http://$bind"
tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/airpaste-macos-hotkey-smoke.XXXXXX")"

server_log="$tmpdir/server.log"
receiver_log="$tmpdir/receiver.log"
publisher_log="$tmpdir/publisher.log"
receiver_cache="$tmpdir/receiver-cache"
original_clip="$(pbpaste 2>/dev/null || true)"
server_log_filter="${AIRPASTE_SMOKE_SERVER_RUST_LOG:-airpaste_server=info}"
agent_log_filter="${AIRPASTE_SMOKE_AGENT_RUST_LOG:-airpaste_agent=info}"

server_pid=""
receiver_pid=""
publisher_pid=""
latest_clip_json=""
peer_base=$((30000 + RANDOM % 20000))
receiver_peer="127.0.0.1:$peer_base"
publisher_peer_port=$((peer_base + 1))
publisher_peer="127.0.0.1:$publisher_peer_port"

stop_pid() {
  local pid="$1"
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
}

cleanup() {
  set +e
  stop_pid "$publisher_pid"
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
  echo "--- publisher log ---" >&2
  cat "$publisher_log" >&2 2>/dev/null || true
}

fail() {
  echo "hotkey smoke failed: $*" >&2
  dump_logs
  exit 1
}

parse_pair_code() {
  sed -n 's/.*"code":"\([^"]*\)".*/\1/p'
}

run_agent() {
  if [ -n "$auth_token" ]; then
    RUST_LOG="$agent_log_filter" "$agent_bin" "$@" --auth-token "$auth_token"
  else
    RUST_LOG="$agent_log_filter" "$agent_bin" "$@"
  fi
}

create_pair_code() {
  local json
  json="$(run_agent \
    --server-url "$base_url" \
    --state-path "$tmpdir/bootstrap.json" \
    --device-name "Mac Hotkey Bootstrap" \
    --create-pair-code \
    --pair-ttl-seconds 600 \
    --publish-clipboard=false \
    --apply-remote=false \
    --remote-paste-hotkey=false)"

  local code
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

wait_for_file_download() {
  local file_name="$1"
  local expected_body="$2"
  local wait_secs="$3"
  local deadline=$((SECONDS + wait_secs))
  local downloaded
  while [ "$SECONDS" -lt "$deadline" ]; do
    downloaded="$(find "$receiver_cache" -type f -name "$file_name" -print -quit 2>/dev/null || true)"
    if [ -n "$downloaded" ] && [ "$(cat "$downloaded")" = "$expected_body" ]; then
      printf "%s" "$downloaded"
      return 0
    fi
    sleep 0.25
  done
  return 1
}

wait_for_state_device_id() {
  local state_path="$1"
  local wait_secs="$2"
  local deadline=$((SECONDS + wait_secs))
  while [ "$SECONDS" -lt "$deadline" ]; do
    if [ -s "$state_path" ] && grep -q '"device_id"' "$state_path"; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

wait_for_log_pattern() {
  local log_path="$1"
  local pattern="$2"
  local wait_secs="$3"
  local deadline=$((SECONDS + wait_secs))
  while [ "$SECONDS" -lt "$deadline" ]; do
    if grep -q "$pattern" "$log_path" 2>/dev/null; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

file_sha256() {
  shasum -a 256 "$1" | awk '{print $1}'
}

first_file_sha256_from_clip() {
  sed -n 's/.*"sha256":"\([0-9a-f][0-9a-f]*\)".*/\1/p' | head -n 1
}

assert_sha256_hex() {
  local value="$1"
  local label="$2"
  if ! printf "%s" "$value" | grep -Eq '^[0-9a-f]{64}$'; then
    fail "$label is not a 64-character lowercase SHA-256 hex digest: $value"
  fi
}

wait_for_latest_file_clip() {
  local wait_secs="$1"
  local deadline=$((SECONDS + wait_secs))
  while [ "$SECONDS" -lt "$deadline" ]; do
    if latest_clip_json="$(run_agent \
      --server-url "$base_url" \
      --state-path "$tmpdir/bootstrap.json" \
      --device-name "Mac Hotkey Bootstrap" \
      --publish-clipboard=false \
      --apply-remote=false \
      --remote-paste-hotkey=false \
      --print-latest-clip)" \
      && printf "%s" "$latest_clip_json" | grep -q '"files"'; then
      return 0
    fi
    sleep 0.25
  done
  return 1
}

ensure_running() {
  local pid="$1"
  local name="$2"
  if ! kill -0 "$pid" 2>/dev/null; then
    fail "$name is not running"
  fi
}

echo "Building Air Paste binaries"
cargo build -p airpaste-server -p airpaste-agent >/dev/null

server_args=(--bind "$bind" --db "$tmpdir/server.redb")
if [ -n "$auth_token" ]; then
  server_args+=(--auth-token "$auth_token")
fi
RUST_LOG="$server_log_filter" "$server_bin" "${server_args[@]}" >"$server_log" 2>&1 &
server_pid=$!
wait_for_health || fail "server did not become healthy at $base_url"

receiver_pair_code="$(create_pair_code)"
run_agent \
  --server-url "$base_url" \
  --state-path "$tmpdir/receiver.json" \
  --device-name "Mac Hotkey Receiver" \
  --pair-code "$receiver_pair_code" \
  --peer-bind "$receiver_peer" \
  --cache-dir "$receiver_cache" \
  --publish-clipboard=false \
  --auto-apply-files=false \
  --auto-paste-files=false \
  --remote-paste-hotkey=true \
  >"$receiver_log" 2>&1 &
receiver_pid=$!
wait_for_state_device_id "$tmpdir/receiver.json" 10 \
  || fail "receiver did not write a device state file"
ensure_running "$receiver_pid" "receiver"
wait_for_log_pattern "$receiver_log" "registered remote paste hotkey Ctrl+Shift+V" 10 \
  || fail "receiver did not register Ctrl+Shift+V hotkey"

publisher_pair_code="$(create_pair_code)"
run_agent \
  --server-url "$base_url" \
  --state-path "$tmpdir/publisher.json" \
  --device-name "Mac Hotkey Publisher" \
  --pair-code "$publisher_pair_code" \
  --peer-bind "$publisher_peer" \
  --peer-public-url "http://127.0.0.1:$publisher_peer_port" \
  --cache-dir "$tmpdir/publisher-cache" \
  --remote-paste-hotkey=false \
  --apply-remote=false \
  --poll-ms 250 \
  >"$publisher_log" 2>&1 &
publisher_pid=$!
sleep 1

source_file="$tmpdir/airpaste-macos-hotkey-source.txt"
source_body="airpaste mac hotkey smoke $(date +%s)"
printf "%s" "$source_body" >"$source_file"
osascript -e "set the clipboard to POSIX file \"$source_file\""

wait_for_latest_file_clip 10 \
  || fail "publisher did not publish a file manifest"
source_sha256="$(file_sha256 "$source_file")"
manifest_sha256="$(printf "%s" "$latest_clip_json" | first_file_sha256_from_clip)"
assert_sha256_hex "$manifest_sha256" "manifest file sha256"
if [ "$manifest_sha256" != "$source_sha256" ]; then
  fail "manifest sha256 $manifest_sha256 did not match source sha256 $source_sha256"
fi
ensure_running "$receiver_pid" "receiver"
sleep 1

cat <<PROMPT

Ready for manual hotkey smoke.

Press Ctrl+Shift+V now.
Waiting up to ${timeout_secs}s for the receiver to download the file...
If it fails, check the receiver log printed below for hotkey registration or WebSocket issues.

PROMPT

downloaded_file="$(wait_for_file_download "$(basename "$source_file")" "$source_body" "$timeout_secs")" \
  || fail "Ctrl+Shift+V did not download the pending file within ${timeout_secs}s"
downloaded_sha256="$(file_sha256 "$downloaded_file")"
if [ "$downloaded_sha256" != "$manifest_sha256" ]; then
  fail "downloaded file sha256 $downloaded_sha256 did not match manifest sha256 $manifest_sha256"
fi

clipboard_info="$(osascript -e 'clipboard info' 2>/dev/null || true)"
case "$clipboard_info" in
  *furl*) ;;
  *) fail "pasteboard does not contain a file URL after hotkey apply: $clipboard_info" ;;
esac

echo "Agent macOS hotkey smoke passed"
echo "Downloaded file: $downloaded_file"
