#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ADDR="${CUA_PERF_ADDR:-127.0.0.1:9876}"
PROFILE="${CUA_PERF_PROFILE:-ci-perf}"
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

"$BIN" --server-addr "$ADDR" --profile "$PROFILE" serve --addr "$ADDR" &
DAEMON_PID="$!"

cleanup() {
  kill "$DAEMON_PID" >/dev/null 2>&1 || true
  wait "$DAEMON_PID" >/dev/null 2>&1 || true
}
trap cleanup EXIT

for _ in $(seq 1 50); do
  if curl -fs "http://$ADDR/healthz" >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done

curl -fsS "http://$ADDR/healthz" >/dev/null
sleep 1

"$BIN" --server-addr "$ADDR" --profile "$PROFILE" perf bench screenshot --iterations 2 --warmup 1 --budget-ms "${CUA_BUDGET_SCREENSHOT_MS:-8000}" --json
"$BIN" --server-addr "$ADDR" --profile "$PROFILE" perf bench stream --iterations 2 --warmup 1 --budget-ms "${CUA_BUDGET_STREAM_MS:-3000}" --json
"$BIN" --server-addr "$ADDR" --profile "$PROFILE" perf bench input --iterations 2 --warmup 1 --budget-ms "${CUA_BUDGET_INPUT_MS:-500}" --json
"$BIN" --server-addr "$ADDR" --profile "$PROFILE" perf bench model-prep --iterations 2 --warmup 1 --budget-ms "${CUA_BUDGET_MODEL_PREP_MS:-8000}" --json
