#!/usr/bin/env bash
set -euo pipefail

export CUA_DEV_HTTP_TOKEN_OVERRIDE=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v curl >/dev/null
command -v jq >/dev/null

RUN_ID="$(date +%s)"
ADDR="${CUA_GUI_SMOKE_ADDR:-127.0.0.1:$((19777 + RUN_ID % 1000))}"
PROFILE="${CUA_GUI_SMOKE_PROFILE:-host-gui-smoke-$RUN_ID}"
TOKEN="${CUA_HTTP_TOKEN:-host-gui-smoke-token-$RUN_ID}"
OUT_DIR="${CUA_GUI_SMOKE_OUT_DIR:-artifacts/cua/macos/gui-smoke-$RUN_ID}"
TRACE_DIR="$OUT_DIR/trace"
OWNER_SESSION_ID="host-gui-smoke-owner"
CUA_HOME_DIR="${CUA_GUI_SMOKE_HOME:-/tmp/cua-gui-smoke-$RUN_ID}"

cargo build -p cua

if [[ -n "${CUA_BIN:-}" ]]; then
  BIN="$CUA_BIN"
elif [[ -x target/debug/cua ]]; then
  BIN="target/debug/cua"
else
  BIN="$(find target -path '*/debug/cua' -type f | head -n 1)"
fi

if [[ -z "$BIN" || ! -x "$BIN" ]]; then
  echo "cua binary not found; run cargo build -p cua first" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"
mkdir -p "$CUA_HOME_DIR"

retry_command() {
  local attempts="$1"
  local delay="$2"
  shift 2
  for attempt in $(seq 1 "$attempts"); do
    if "$@"; then
      return 0
    fi
    if [[ "$attempt" -lt "$attempts" ]]; then
      sleep "$delay"
    fi
  done
  return 1
}

CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" CUA_TRACE_DIR="$TRACE_DIR" "$BIN" --server-addr "$ADDR" --profile "$PROFILE" serve --addr "$ADDR" &
DAEMON_PID="$!"

cleanup() {
  kill "$DAEMON_PID" >/dev/null 2>&1 || true
  wait "$DAEMON_PID" >/dev/null 2>&1 || true
}
trap cleanup EXIT

for _ in $(seq 1 80); do
  if curl -fs "http://$ADDR/healthz" >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done

curl -fsS "http://$ADDR/healthz" >/dev/null
sleep 1

CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" "$BIN" --server-addr "$ADDR" --profile "$PROFILE" status --json > "$OUT_DIR/status.json"
CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" "$BIN" --server-addr "$ADDR" --profile "$PROFILE" observe --json > "$OUT_DIR/desktop.json"
retry_command 3 1 env CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" "$BIN" --server-addr "$ADDR" --profile "$PROFILE" screenshot --out "$OUT_DIR/screen.png" --json > "$OUT_DIR/screenshot.json"
CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" "$BIN" --server-addr "$ADDR" --profile "$PROFILE" session acquire "$OWNER_SESSION_ID" --role owner --client-name host-gui-smoke --ttl-ms 60000 --json > "$OUT_DIR/owner-session.json"

x="$(jq '.cursor.x | round' "$OUT_DIR/desktop.json")"
y="$(jq '.cursor.y | round' "$OUT_DIR/desktop.json")"
CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" "$BIN" --server-addr "$ADDR" --profile "$PROFILE" mouse --session-id "$OWNER_SESSION_ID" move "$x" "$y" > "$OUT_DIR/mouse-move.json"

sleep 1
CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" "$BIN" trace verify "$TRACE_DIR" --json > "$OUT_DIR/trace-verify.json"

jq -e '
  .permissions.portal == "not_applicable" and
  .schema_version == "cua.v1" and
  .status == "ready"
' "$OUT_DIR/status.json" >/dev/null

jq -e '
  (.displays | length) > 0 and
  (.windows | type) == "array" and
  (.cursor.x | type) == "number" and
  (.cursor.y | type) == "number"
' "$OUT_DIR/desktop.json" >/dev/null

jq -e '
  .width > 0 and
  .height > 0 and
  .byte_len > 0
' "$OUT_DIR/screenshot.json" >/dev/null

jq -e '
  .accepted == true and
  .owner_session_id == "host-gui-smoke-owner"
' "$OUT_DIR/owner-session.json" >/dev/null

jq -e '
  .effect == "unverifiable" and
  .route == "accessibility" and
  .delivery_mode == "desktop"
' "$OUT_DIR/mouse-move.json" >/dev/null

jq -e '
  .ok == true and
  .action_turns >= 1 and
  (.missing_artifacts | length) == 0
' "$OUT_DIR/trace-verify.json" >/dev/null

echo "$OUT_DIR"
