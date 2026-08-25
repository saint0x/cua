#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ADDR="${CUA_VOICE_SMOKE_ADDR:-127.0.0.1:9878}"
PROFILE="${CUA_VOICE_SMOKE_PROFILE:-host-voice-smoke}"

cargo build -p cua-voice

if [[ -n "${CUA_VOICE_BIN:-}" ]]; then
  BIN="$CUA_VOICE_BIN"
elif [[ -x target/debug/cua-voice ]]; then
  BIN="target/debug/cua-voice"
else
  BIN="$(find target -path '*/debug/cua-voice' -type f | head -n 1)"
fi

if [[ -z "$BIN" || ! -x "$BIN" ]]; then
  echo "cua-voice binary not found" >&2
  exit 1
fi

OPENROUTER_API_KEY="${OPENROUTER_API_KEY:-smoke-only}" "$BIN" \
  --server-addr "$ADDR" \
  --profile "$PROFILE" \
  --record-ms 250 &
PID="$!"

cleanup() {
  kill "$PID" >/dev/null 2>&1 || true
  wait "$PID" >/dev/null 2>&1 || true
}
trap cleanup EXIT

sleep 2
if ! kill -0 "$PID" >/dev/null 2>&1; then
  echo "cua-voice exited during startup smoke" >&2
  exit 1
fi

echo "cua-voice startup ok"
