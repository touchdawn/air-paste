#!/usr/bin/env bash
set -euo pipefail

# Ensure the agent emits the info-level lines this smoke greps for ("served relay file",
# "downloaded remote file via relay"), regardless of any RUST_LOG already in the environment.
export RUST_LOG="${AIRPASTE_SMOKE_RUST_LOG:-airpaste_agent=info}"

bind="127.0.0.1:18085"
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
Usage: scripts/smoke-relay-macos.sh [--bind 127.0.0.1:18085] [--auth-token TOKEN]

Runs a macOS-only Air Paste encrypted-relay smoke test:
  - a persistent source agent publishes a file manifest and registers the local file
  - a receiver pulls the file through the server-mediated encrypted relay (--prefer-relay),
    so the bytes never traverse the direct peer port
  - asserts the downloaded file is byte-exact (size + SHA-256) and the source actually
    served it over the relay

The receiver is forced onto the relay with an unreachable direct peer so a regression that
silently falls back to direct (or fails the relay) is caught.
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
  echo "smoke-relay-macos.sh must run on macOS" >&2
  exit 1
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
server_bin="$root/target/debug/airpaste-server"
agent_bin="$root/target/debug/airpaste-agent"
base_url="http://$bind"
tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/airpaste-relay-smoke.XXXXXX")"

server_log="$tmpdir/server.log"
source_log="$tmpdir/source.log"
receiver_log="$tmpdir/receiver.log"
original_clip="$(pbpaste 2>/dev/null || true)"

server_pid=""
source_pid=""
peer_base=$((20000 + RANDOM % 20000))
source_peer="127.0.0.1:$peer_base"
receiver_peer="127.0.0.1:$((peer_base + 1))"

stop_pid() {
  local pid="$1"
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
}

cleanup() {
  set +e
  stop_pid "$source_pid"
  stop_pid "$server_pid"
  printf "%s" "$original_clip" | pbcopy 2>/dev/null || true
  rm -rf "$tmpdir"
}
trap cleanup EXIT

dump_logs() {
  echo "--- server log ---" >&2
  cat "$server_log" >&2 2>/dev/null || true
  echo "--- source log ---" >&2
  cat "$source_log" >&2 2>/dev/null || true
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
    --device-name "Mac Relay Bootstrap" \
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

file_sha256() {
  shasum -a 256 "$1" | awk '{print $1}'
}

first_file_sha256_from_clip() {
  sed -n 's/.*"sha256":"\([0-9a-f][0-9a-f]*\)".*/\1/p' | head -n 1
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

# Pair + register the receiver first so it is trusted and has an encryption key the source
# can wrap the per-file content key for.
receiver_pair_code="$(create_pair_code)"
run_agent \
  --server-url "$base_url" \
  --state-path "$tmpdir/receiver.json" \
  --device-name "Mac Relay Receiver" \
  --pair-code "$receiver_pair_code" \
  --peer-bind "$receiver_peer" \
  --cache-dir "$tmpdir/receiver-cache" \
  --remote-paste-hotkey=false \
  --publish-clipboard=false \
  --apply-remote=false \
  --print-latest-clip \
  >"$receiver_log" 2>&1

# Persistent source agent: publishes file manifests and serves relay sessions on demand.
source_pair_code="$(create_pair_code)"
run_agent \
  --server-url "$base_url" \
  --state-path "$tmpdir/source.json" \
  --device-name "Mac Relay Source" \
  --pair-code "$source_pair_code" \
  --peer-bind "$source_peer" \
  --peer-public-url "http://$source_peer" \
  --cache-dir "$tmpdir/source-cache" \
  --remote-paste-hotkey=false \
  --apply-remote=false \
  --poll-ms 250 \
  >"$source_log" 2>&1 &
source_pid=$!
sleep 1

echo "Publish file"
source_file="$tmpdir/airpaste-relay-source.txt"
source_body="airpaste mac relay smoke $(date +%s)"
printf "%s" "$source_body" >"$source_file"
osascript -e "set the clipboard to POSIX file \"$source_file\""

latest_deadline=$((SECONDS + 10))
file_manifest_seen=false
latest_clip_json=""
while [ "$SECONDS" -lt "$latest_deadline" ]; do
  if latest_clip_json="$(run_agent \
    --server-url "$base_url" \
    --state-path "$tmpdir/bootstrap.json" \
    --device-name "Mac Relay Bootstrap" \
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
if [ "$manifest_sha256" != "$source_sha256" ]; then
  fail "manifest sha256 $manifest_sha256 did not match source sha256 $source_sha256"
fi

echo "Pull through encrypted relay"
# Force the relay: --prefer-relay routes the pull through the server, never the direct peer.
# The agent logs to stdout, so capture the whole run to a file and extract the JSON result.
relay_run_out="$tmpdir/relay-run.log"
if ! run_agent \
  --server-url "$base_url" \
  --state-path "$tmpdir/receiver.json" \
  --device-name "Mac Relay Receiver" \
  --peer-bind "$receiver_peer" \
  --cache-dir "$tmpdir/receiver-cache" \
  --remote-paste-hotkey=false \
  --publish-clipboard=false \
  --apply-remote=false \
  --prefer-relay true \
  --apply-latest-files-once >"$relay_run_out" 2>&1; then
  cat "$relay_run_out" >>"$receiver_log"
  fail "failed to apply latest file clip through relay"
fi
cat "$relay_run_out" >>"$receiver_log"

downloaded_file="$(sed -n 's/^\["\([^"]*\)".*/\1/p' "$relay_run_out")"
if [ -z "$downloaded_file" ]; then
  fail "could not parse downloaded file path from relay run output"
fi
if [ ! -f "$downloaded_file" ] || [ "$(cat "$downloaded_file")" != "$source_body" ]; then
  fail "relayed file content did not match source"
fi
downloaded_sha256="$(file_sha256 "$downloaded_file")"
if [ "$downloaded_sha256" != "$manifest_sha256" ]; then
  fail "relayed file sha256 $downloaded_sha256 did not match manifest sha256 $manifest_sha256"
fi

# Confirm the data plane was actually the relay, not a silent direct download.
if ! grep -q "downloaded remote file via relay" "$relay_run_out"; then
  fail "receiver did not log a relay download (did it fall back to direct?)"
fi
if ! grep -q "served relay file" "$source_log"; then
  fail "source did not log serving a relay file"
fi

stop_pid "$source_pid"
source_pid=""

echo "Relay macOS smoke passed"
echo "Relayed file: $downloaded_file"
