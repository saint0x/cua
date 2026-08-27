#!/usr/bin/env bash
set -euo pipefail

export CUA_DEV_HTTP_TOKEN_OVERRIDE=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

cargo build -p cua -p cua-voice

if [[ -n "${CUA_BIN:-}" ]]; then
  CUA_BIN_PATH="$CUA_BIN"
elif [[ -x target/debug/cua ]]; then
  CUA_BIN_PATH="target/debug/cua"
else
  CUA_BIN_PATH="$(find target -path '*/debug/cua' -type f 2>/dev/null | head -n 1)"
fi

if [[ -z "$CUA_BIN_PATH" || ! -x "$CUA_BIN_PATH" ]]; then
  echo "cua binary not found" >&2
  exit 1
fi

RUN_ID="$(date +%s)"
PROFILE="${CUA_SINGLETON_PROOF_PROFILE:-singleton-proof-$RUN_ID}"
ADDR1="${CUA_SINGLETON_PROOF_ADDR1:-127.0.0.1:$((28000 + RUN_ID % 1000))}"
ADDR2="${CUA_SINGLETON_PROOF_ADDR2:-127.0.0.1:$((29000 + RUN_ID % 1000))}"
TOKEN="${CUA_SINGLETON_PROOF_TOKEN:-singleton-proof-token-$RUN_ID}"
OUT_DIR="${CUA_SINGLETON_PROOF_OUT_DIR:-artifacts/cua/singleton-proof-$RUN_ID}"
mkdir -p "$OUT_DIR"

CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --profile "$PROFILE" \
  serve --addr "$ADDR1" --hud-mode headless >"$OUT_DIR/daemon1.log" 2>&1 &
PID1="$!"

cleanup() {
  kill "$PID1" >/dev/null 2>&1 || true
  wait "$PID1" >/dev/null 2>&1 || true
}
trap cleanup EXIT

for _ in $(seq 1 100); do
  if CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --profile "$PROFILE" \
    --server-addr "$ADDR1" status --json >"$OUT_DIR/status1.json" 2>/dev/null; then
    break
  fi
  sleep 0.05
done
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --profile "$PROFILE" \
  --server-addr "$ADDR1" status --json >"$OUT_DIR/status1.json"

set +e
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --profile "$PROFILE" \
  serve --addr "$ADDR2" --hud-mode headless >"$OUT_DIR/daemon2.log" 2>&1
SECOND_CODE="$?"
set -e

CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --profile "$PROFILE" \
  --server-addr "$ADDR1" status --json >"$OUT_DIR/status-after.json"

SOCKET="$HOME/.cua/profiles/$PROFILE/daemon.sock"
python3 - "$OUT_DIR" "$PID1" "$SECOND_CODE" "$SOCKET" <<'PY'
import json
import os
import socket
import sys

out_dir, pid1, second_code, socket_path = sys.argv[1:5]
with open(os.path.join(out_dir, "status1.json"), encoding="utf-8") as handle:
    before = json.load(handle)
with open(os.path.join(out_dir, "status-after.json"), encoding="utf-8") as handle:
    after = json.load(handle)
with open(os.path.join(out_dir, "daemon2.log"), encoding="utf-8", errors="replace") as handle:
    second_log = handle.read()

socket_still_connects = False
stream = None
try:
    stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    stream.settimeout(0.2)
    stream.connect(socket_path)
    socket_still_connects = True
finally:
    if stream is not None:
        stream.close()

proof = {
    "schema_version": "cua.singleton_proof.v1",
    "profile": before["profile"],
    "first_pid": int(pid1),
    "second_exit_code": int(second_code),
    "second_refused_live_socket": int(second_code) != 0 and "already running" in second_log,
    "socket_still_connects": socket_still_connects,
    "started_at_stable": before["started_at"] == after["started_at"],
    "active_profile": after["active_profile"],
}
proof["ok"] = (
    proof["second_refused_live_socket"]
    and proof["socket_still_connects"]
    and proof["started_at_stable"]
)
with open(os.path.join(out_dir, "proof.json"), "w", encoding="utf-8") as handle:
    json.dump(proof, handle, indent=2)
    handle.write("\n")
print(json.dumps(proof, indent=2))
if not proof["ok"]:
    raise SystemExit(1)
PY
