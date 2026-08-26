#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

cargo build -p cua

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
PROFILE="${CUA_SESSION_PROOF_PROFILE:-session-proof-$RUN_ID}"
ADDR="${CUA_SESSION_PROOF_ADDR:-127.0.0.1:$((30000 + RUN_ID % 1000))}"
TOKEN="${CUA_SESSION_PROOF_TOKEN:-session-proof-token-$RUN_ID}"
OUT_DIR="${CUA_SESSION_PROOF_OUT_DIR:-artifacts/cua/session-proof-$RUN_ID}"
SOCKET="$HOME/.cua/profiles/$PROFILE/daemon.sock"
mkdir -p "$OUT_DIR"

CUA_HTTP_TOKEN="$TOKEN" CUA_HUD_AUTOSTART=0 "$CUA_BIN_PATH" --profile "$PROFILE" \
  serve --addr "$ADDR" --hud-mode headless >"$OUT_DIR/daemon.log" 2>&1 &
PID="$!"

cleanup() {
  kill "$PID" >/dev/null 2>&1 || true
  wait "$PID" >/dev/null 2>&1 || true
}
trap cleanup EXIT

for _ in $(seq 1 100); do
  if [[ -S "$SOCKET" ]] && CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" \
    --profile "$PROFILE" --server-addr "$ADDR" status --json >"$OUT_DIR/status.json" 2>/dev/null; then
    break
  fi
  sleep 0.05
done
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --profile "$PROFILE" \
  --server-addr "$ADDR" status --json >"$OUT_DIR/status.json"

python3 - "$OUT_DIR" "$SOCKET" "$TOKEN" <<'PY'
import json
import os
import socket
import sys
import uuid

out_dir, socket_path, token = sys.argv[1:4]

def call(method, params=None, session_id=None):
    request = {
        "id": str(uuid.uuid4()),
        "token": token,
        "method": method,
        "params": params or {},
    }
    if session_id is not None:
        request["session_id"] = session_id
    stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    stream.settimeout(2)
    try:
        stream.connect(socket_path)
        stream.sendall((json.dumps(request) + "\n").encode("utf-8"))
        data = b""
        while not data.endswith(b"\n"):
            chunk = stream.recv(65536)
            if not chunk:
                break
            data += chunk
    finally:
        stream.close()
    return json.loads(data.decode("utf-8"))

owner = call(
    "session.acquire",
    {
        "schema_version": "cua.v1",
        "session_id": "owner-proof",
        "client_name": "host session proof owner",
        "role": "owner",
        "ttl_ms": 60000,
    },
)
observer = call(
    "session.acquire",
    {
        "schema_version": "cua.v1",
        "session_id": "observer-proof",
        "client_name": "host session proof observer",
        "role": "observer",
    },
)
anonymous_pause = call("control.pause")
observer_pause = call("control.pause", session_id="observer-proof")
owner_pause = call("control.pause", session_id="owner-proof")
status = call("session.status")
cancel_observer = call(
    "session.cancel",
    {
        "schema_version": "cua.v1",
        "session_id": "owner-proof",
        "target_session_id": "observer-proof",
    },
)

proof = {
    "schema_version": "cua.session_proof.v1",
    "owner_accepted": owner.get("ok") is True
    and owner["result"]["owner_session_id"] == "owner-proof",
    "observer_accepted": observer.get("ok") is True
    and observer["result"]["session"]["role"] == "observer",
    "anonymous_write_refused": anonymous_pause.get("ok") is False
    and anonymous_pause["error"]["code"] == "session_owner",
    "observer_write_refused": observer_pause.get("ok") is False
    and observer_pause["error"]["code"] == "session_owner",
    "owner_write_confirmed": owner_pause.get("ok") is True
    and owner_pause["result"]["safety_state"] == "paused",
    "inventory_owner": status.get("ok") is True
    and status["result"]["owner_session_id"] == "owner-proof",
    "inventory_connected_clients": status.get("ok") is True
    and status["result"]["connected_clients"] == 2,
    "owner_cancelled_observer": cancel_observer.get("ok") is True
    and cancel_observer["result"]["connected_clients"] == 1,
    "artifacts": {
        "owner": owner,
        "observer": observer,
        "anonymous_pause": anonymous_pause,
        "observer_pause": observer_pause,
        "owner_pause": owner_pause,
        "status": status,
        "cancel_observer": cancel_observer,
    },
}
proof["ok"] = all(value for key, value in proof.items() if key not in {"schema_version", "artifacts"})
with open(os.path.join(out_dir, "proof.json"), "w", encoding="utf-8") as handle:
    json.dump(proof, handle, indent=2)
    handle.write("\n")
print(json.dumps(proof, indent=2))
if not proof["ok"]:
    raise SystemExit(1)
PY

CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --profile "$PROFILE" --server-addr "$ADDR" \
  session status --json >"$OUT_DIR/cli-session-status.json"
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --profile "$PROFILE" --server-addr "$ADDR" \
  session acquire cli-observer --role observer --client-name "cli session proof" --json \
  >"$OUT_DIR/cli-session-acquire.json"
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --profile "$PROFILE" --server-addr "$ADDR" \
  session cancel owner-proof --target-session-id cli-observer --json \
  >"$OUT_DIR/cli-session-cancel.json"

python3 - "$OUT_DIR" <<'PY'
import json
import os
import sys

out_dir = sys.argv[1]
with open(os.path.join(out_dir, "proof.json"), encoding="utf-8") as handle:
    proof = json.load(handle)
with open(os.path.join(out_dir, "cli-session-status.json"), encoding="utf-8") as handle:
    cli_status = json.load(handle)
with open(os.path.join(out_dir, "cli-session-acquire.json"), encoding="utf-8") as handle:
    cli_acquire = json.load(handle)
with open(os.path.join(out_dir, "cli-session-cancel.json"), encoding="utf-8") as handle:
    cli_cancel = json.load(handle)

proof["cli_status_reports_owner"] = cli_status["owner_session_id"] == "owner-proof"
proof["cli_observer_accepted"] = cli_acquire["accepted"] is True and cli_acquire["session"]["role"] == "observer"
proof["cli_cancelled_observer"] = cli_cancel["owner_session_id"] == "owner-proof" and all(
    session["session_id"] != "cli-observer" for session in cli_cancel["sessions"]
)
proof["artifacts"]["cli_status"] = cli_status
proof["artifacts"]["cli_acquire"] = cli_acquire
proof["artifacts"]["cli_cancel"] = cli_cancel
proof["ok"] = all(
    value
    for key, value in proof.items()
    if key not in {"schema_version", "artifacts"}
)
with open(os.path.join(out_dir, "proof.json"), "w", encoding="utf-8") as handle:
    json.dump(proof, handle, indent=2)
    handle.write("\n")
print(json.dumps(proof, indent=2))
if not proof["ok"]:
    raise SystemExit(1)
PY
