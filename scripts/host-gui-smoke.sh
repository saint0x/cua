#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v curl >/dev/null
command -v jq >/dev/null

ADDR="${CUA_GUI_SMOKE_ADDR:-127.0.0.1:9877}"
PROFILE="${CUA_GUI_SMOKE_PROFILE:-host-gui-smoke}"
TOKEN="${CUA_HTTP_TOKEN:-host-gui-smoke-token}"
RUN_ID="$(date +%s)"
OUT_DIR="${CUA_GUI_SMOKE_OUT_DIR:-artifacts/cua/macos/gui-smoke-$RUN_ID}"
TRACE_DIR="$OUT_DIR/trace"

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

CUA_HTTP_TOKEN="$TOKEN" CUA_TRACE_DIR="$TRACE_DIR" "$BIN" --server-addr "$ADDR" --profile "$PROFILE" serve --addr "$ADDR" &
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

CUA_HTTP_TOKEN="$TOKEN" "$BIN" --server-addr "$ADDR" --profile "$PROFILE" status --json > "$OUT_DIR/status.json"
CUA_HTTP_TOKEN="$TOKEN" "$BIN" --server-addr "$ADDR" --profile "$PROFILE" observe --json > "$OUT_DIR/desktop.json"
CUA_HTTP_TOKEN="$TOKEN" "$BIN" --server-addr "$ADDR" --profile "$PROFILE" screenshot --out "$OUT_DIR/screen.png" --json > "$OUT_DIR/screenshot.json"

x="$(jq '.cursor.x | round' "$OUT_DIR/desktop.json")"
y="$(jq '.cursor.y | round' "$OUT_DIR/desktop.json")"
CUA_HTTP_TOKEN="$TOKEN" "$BIN" --server-addr "$ADDR" --profile "$PROFILE" mouse move "$x" "$y" > "$OUT_DIR/mouse-move.json"

sleep 1
CUA_HTTP_TOKEN="$TOKEN" "$BIN" trace verify "$TRACE_DIR" --json > "$OUT_DIR/trace-verify.json"

jq -e '
  .permissions.portal == "not_applicable" and
  .schema_version == "cua.v1" and
  .status == "degraded"
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
  .effect == "confirmed" and
  .route == "accessibility"
' "$OUT_DIR/mouse-move.json" >/dev/null

jq -e '
  .ok == true and
  .action_turns >= 1 and
  (.missing_artifacts | length) == 0
' "$OUT_DIR/trace-verify.json" >/dev/null

echo "$OUT_DIR"
