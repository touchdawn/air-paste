#!/usr/bin/env bash
set -euo pipefail

bind="127.0.0.1:18083"
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
Usage: scripts/smoke-agent-macos.sh [--bind 127.0.0.1:18083] [--auth-token TOKEN]

Runs a macOS-only Air Paste agent smoke test:
  - text publish/apply through the server and WebSocket
  - file URL publish, signed peer download, and file URL pasteboard apply

The script uses temporary state/cache files and restores the text clipboard on exit.
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
  echo "smoke-agent-macos.sh must run on macOS" >&2
  exit 1
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
server_bin="$root/target/debug/airpaste-server"
agent_bin="$root/target/debug/airpaste-agent"
base_url="http://$bind"
tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/airpaste-macos-smoke.XXXXXX")"

server_log="$tmpdir/server.log"
text_receiver_log="$tmpdir/text-receiver.log"
file_receiver_log="$tmpdir/file-receiver.log"
file_publisher_log="$tmpdir/file-publisher.log"
original_clip="$(pbpaste 2>/dev/null || true)"

server_pid=""
text_receiver_pid=""
file_publisher_pid=""
peer_base=$((20000 + RANDOM % 20000))
text_receiver_peer="127.0.0.1:$peer_base"
file_publisher_peer_port=$((peer_base + 1))
file_publisher_peer="127.0.0.1:$file_publisher_peer_port"
file_receiver_peer="127.0.0.1:$((peer_base + 2))"

stop_pid() {
  local pid="$1"
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
}

cleanup() {
  set +e
  stop_pid "$file_publisher_pid"
  stop_pid "$text_receiver_pid"
  stop_pid "$server_pid"
  printf "%s" "$original_clip" | pbcopy 2>/dev/null || true
  rm -rf "$tmpdir"
}
trap cleanup EXIT

dump_logs() {
  echo "--- server log ---" >&2
  cat "$server_log" >&2 2>/dev/null || true
  echo "--- text receiver log ---" >&2
  cat "$text_receiver_log" >&2 2>/dev/null || true
  echo "--- file receiver log ---" >&2
  cat "$file_receiver_log" >&2 2>/dev/null || true
  echo "--- file publisher log ---" >&2
  cat "$file_publisher_log" >&2 2>/dev/null || true
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
  local json
  json="$(run_agent \
    --server-url "$base_url" \
    --state-path "$tmpdir/bootstrap.json" \
    --device-name "Mac Smoke Bootstrap" \
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

wait_for_text_clipboard() {
  local expected="$1"
  local deadline=$((SECONDS + 10))
  local actual
  while [ "$SECONDS" -lt "$deadline" ]; do
    actual="$(pbpaste 2>/dev/null || true)"
    if [ "$actual" = "$expected" ]; then
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

echo "Building Air Paste binaries"
cargo build -p airpaste-server -p airpaste-agent >/dev/null

server_args=(--bind "$bind" --db "$tmpdir/server.redb")
if [ -n "$auth_token" ]; then
  server_args+=(--auth-token "$auth_token")
fi
"$server_bin" "${server_args[@]}" >"$server_log" 2>&1 &
server_pid=$!
wait_for_health || fail "server did not become healthy at $base_url"

pair_code="$(create_pair_code)"

echo "Text sync"
run_agent \
  --server-url "$base_url" \
  --state-path "$tmpdir/text-receiver.json" \
  --device-name "Mac Smoke Text Receiver" \
  --pair-code "$pair_code" \
  --peer-bind "$text_receiver_peer" \
  --cache-dir "$tmpdir/text-cache" \
  --remote-paste-hotkey=false \
  --publish-clipboard=false \
  >"$text_receiver_log" 2>&1 &
text_receiver_pid=$!
sleep 1

text_body="airpaste mac text smoke $(date +%s)"
run_agent \
  --server-url "$base_url" \
  --state-path "$tmpdir/bootstrap.json" \
  --device-name "Mac Smoke Bootstrap" \
  --publish-text-once "$text_body" \
  --publish-clipboard=false \
  --apply-remote=false \
  --remote-paste-hotkey=false \
  >/dev/null
wait_for_text_clipboard "$text_body" || fail "text was not applied to pasteboard"
stop_pid "$text_receiver_pid"
text_receiver_pid=""

echo "File sync"
file_receiver_pair_code="$(create_pair_code)"
run_agent \
  --server-url "$base_url" \
  --state-path "$tmpdir/file-receiver.json" \
  --device-name "Mac Smoke File Receiver" \
  --pair-code "$file_receiver_pair_code" \
  --peer-bind "$file_receiver_peer" \
  --cache-dir "$tmpdir/file-receiver-cache" \
  --remote-paste-hotkey=false \
  --publish-clipboard=false \
  --apply-remote=false \
  --print-latest-clip \
  >"$file_receiver_log" 2>&1

file_publisher_pair_code="$(create_pair_code)"
run_agent \
  --server-url "$base_url" \
  --state-path "$tmpdir/file-publisher.json" \
  --device-name "Mac Smoke File Publisher" \
  --pair-code "$file_publisher_pair_code" \
  --peer-bind "$file_publisher_peer" \
  --peer-public-url "http://127.0.0.1:$file_publisher_peer_port" \
  --cache-dir "$tmpdir/file-publisher-cache" \
  --remote-paste-hotkey=false \
  --apply-remote=false \
  --poll-ms 250 \
  >"$file_publisher_log" 2>&1 &
file_publisher_pid=$!
sleep 1

source_file="$tmpdir/airpaste-macos-source.txt"
source_body="airpaste mac file smoke $(date +%s)"
printf "%s" "$source_body" >"$source_file"
osascript -e "set the clipboard to POSIX file \"$source_file\""

latest_deadline=$((SECONDS + 10))
file_manifest_seen=false
latest_clip_json=""
while [ "$SECONDS" -lt "$latest_deadline" ]; do
  if latest_clip_json="$(run_agent \
    --server-url "$base_url" \
    --state-path "$tmpdir/bootstrap.json" \
    --device-name "Mac Smoke Bootstrap" \
    --publish-clipboard=false \
    --apply-remote=false \
    --remote-paste-hotkey=false \
    --print-latest-clip)" \
    && printf "%s" "$latest_clip_json" | grep -q '"files"'; then
    file_manifest_seen=true
    break
  fi
  sleep 0.25
done
if [ "$file_manifest_seen" != true ]; then
  fail "file manifest was not published"
fi
source_sha256="$(file_sha256 "$source_file")"
manifest_sha256="$(printf "%s" "$latest_clip_json" | first_file_sha256_from_clip)"
assert_sha256_hex "$manifest_sha256" "manifest file sha256"
if [ "$manifest_sha256" != "$source_sha256" ]; then
  fail "manifest sha256 $manifest_sha256 did not match source sha256 $source_sha256"
fi

downloaded_json="$(run_agent \
  --server-url "$base_url" \
  --state-path "$tmpdir/file-receiver.json" \
  --device-name "Mac Smoke File Receiver" \
  --peer-bind "$file_receiver_peer" \
  --cache-dir "$tmpdir/file-receiver-cache" \
  --remote-paste-hotkey=false \
  --publish-clipboard=false \
  --apply-remote=false \
  --apply-latest-files-once)" || fail "failed to apply latest file clip"
downloaded_file="$(printf "%s" "$downloaded_json" | sed -n 's/^\["\([^"]*\)".*/\1/p')"
if [ -z "$downloaded_file" ]; then
  fail "could not parse downloaded file path from $downloaded_json"
fi
if [ ! -f "$downloaded_file" ] || [ "$(cat "$downloaded_file")" != "$source_body" ]; then
  fail "downloaded file content did not match source"
fi
downloaded_sha256="$(file_sha256 "$downloaded_file")"
if [ "$downloaded_sha256" != "$manifest_sha256" ]; then
  fail "downloaded file sha256 $downloaded_sha256 did not match manifest sha256 $manifest_sha256"
fi
clipboard_info="$(osascript -e 'clipboard info' 2>/dev/null || true)"
case "$clipboard_info" in
  *furl*) ;;
  *) fail "pasteboard does not contain a file URL after file apply: $clipboard_info" ;;
esac

stop_pid "$file_publisher_pid"
file_publisher_pid=""

echo "Agent macOS smoke passed"
echo "Downloaded file: $downloaded_file"
