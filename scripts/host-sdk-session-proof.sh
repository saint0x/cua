#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v jq >/dev/null
command -v python3 >/dev/null

cargo build -p cua >/dev/null

debug_bin() {
  local name="$1"
  local target_root="${CARGO_TARGET_DIR:-target}"
  if [[ -x "$target_root/debug/$name" ]]; then
    printf '%s\n' "$target_root/debug/$name"
  elif [[ -x "target/debug/$name" ]]; then
    printf '%s\n' "target/debug/$name"
  else
    local root found
    for root in "$target_root" target; do
      [[ -d "$root" ]] || continue
      found="$(find "$root" -path "*/debug/$name" -type f 2>/dev/null | head -n 1 || true)"
      if [[ -n "$found" ]]; then
        printf '%s\n' "$found"
        return 0
      fi
    done
  fi
}

if [[ -n "${CUA_BIN:-}" ]]; then
  CUA_BIN_PATH="$CUA_BIN"
else
  CUA_BIN_PATH="$(debug_bin cua)"
fi

if [[ -z "${CUA_BIN_PATH:-}" || ! -x "$CUA_BIN_PATH" ]]; then
  cargo build -p cua
  CUA_BIN_PATH="${CUA_BIN:-$(debug_bin cua)}"
fi
if [[ ! -x "$CUA_BIN_PATH" ]]; then
  echo "cua binary not found" >&2
  exit 1
fi

RUN_ID="$(date +%s)"
PROFILE="${CUA_SDK_PROOF_PROFILE:-sdk-session-proof-$RUN_ID}"
ADDR="${CUA_SDK_PROOF_ADDR:-127.0.0.1:$((32000 + RUN_ID % 1000))}"
TOKEN="${CUA_SDK_PROOF_TOKEN:-sdk-session-proof-token-$RUN_ID}"
OUT_DIR="${CUA_SDK_PROOF_OUT_DIR:-artifacts/cua/sdk-session-proof-$RUN_ID}"
CUA_HOME_DIR="${CUA_SDK_PROOF_CUA_HOME:-$OUT_DIR/cua-home}"
SOCKET="$CUA_HOME_DIR/profiles/$PROFILE/daemon.sock"
PROOF="$OUT_DIR/proof.json"
mkdir -p "$OUT_DIR"

CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" CUA_HUD_AUTOSTART=0 \
  "$CUA_BIN_PATH" --profile "$PROFILE" serve --addr "$ADDR" --hud-mode headless \
  > "$OUT_DIR/daemon.log" 2>&1 &
PID="$!"

cleanup() {
  kill "$PID" >/dev/null 2>&1 || true
  wait "$PID" >/dev/null 2>&1 || true
}
trap cleanup EXIT

for _ in $(seq 1 120); do
  if [[ -S "$SOCKET" ]] && CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" \
    "$CUA_BIN_PATH" --profile "$PROFILE" status --json > "$OUT_DIR/status-ready.json" 2>/dev/null; then
    break
  fi
  sleep 0.05
done
CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" \
  "$CUA_BIN_PATH" --profile "$PROFILE" status --json > "$OUT_DIR/status-ready.json"

PYTHONPATH="$ROOT/sdks/python" CUA_BIN="$CUA_BIN_PATH" CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" \
  python3 - "$PROFILE" "$CUA_BIN_PATH" "$PROOF" <<'PY'
import json
import os
import sys

from cua_sdk import Cua

profile, bin_path, proof_path = sys.argv[1:4]
sdk = Cua.connect(profile=profile, bin=bin_path, env={
    "CUA_HOME": os.environ["CUA_HOME"],
    "CUA_HTTP_TOKEN": os.environ["CUA_HTTP_TOKEN"],
})

def try_call(fn):
    try:
        return {"ok": True, "value": fn()}
    except Exception as error:
        return {"ok": False, "error": str(error)}

status = sdk.status()
context = sdk.context(max_width=320, encoding="png", force_fresh=True, include_bytes=False)
owner = sdk.acquire_owner("python sdk owner proof", ttl_ms=60000)
observer_report = sdk.rpc(
    "session.acquire",
    {
        "schema_version": "cua.v1",
        "session_id": "python-sdk-observer-proof",
        "client_name": "python sdk observer proof",
        "role": "observer",
        "ttl_ms": 60000,
    },
)
observer = observer_report

anonymous_pause = try_call(lambda: sdk.rpc("control.pause"))
observer_pause = try_call(lambda: sdk.rpc("control.pause", session_id=observer["session"]["session_id"]))
owner_pause = try_call(lambda: sdk.pause(owner))
owner_resume = try_call(lambda: sdk.resume(owner))
owner_dispatch = try_call(
    lambda: sdk.dispatch(
        {"kind": "shell_exec", "command": "printf sdk-owner-dispatch", "timeout_ms": 1000},
        session=owner,
    )
)
observer_status = sdk.session_status()

proof = {
    "schema_version": "cua.sdk_session_proof.v1",
    "status_ok": status["schema_version"] == "cua.v1" and status["profile"] == profile,
    "context_ok": context["schema_version"] == "cua.v1" and context["frame"]["envelope"]["width"] > 0,
    "owner_acquired": owner.raw["accepted"] is True and owner.raw["session"]["role"] == "owner",
    "observer_acquired": observer["accepted"] is True and observer["session"]["role"] == "observer",
    "anonymous_write_refused": anonymous_pause["ok"] is False and "session_owner" in anonymous_pause["error"],
    "observer_write_refused": observer_pause["ok"] is False and "session_owner" in observer_pause["error"],
    "owner_pause_confirmed": owner_pause["ok"] is True and owner_pause["value"]["safety_state"] == "paused",
    "owner_resume_confirmed": owner_resume["ok"] is True and owner_resume["value"]["safety_state"] == "running",
    "owner_dispatch_confirmed": owner_dispatch["ok"] is True
    and owner_dispatch["value"]["effect"] == "confirmed",
    "observer_read_only_status": observer_status["owner_session_id"] == owner.session_id,
    "artifacts": {
        "status": status,
        "context_envelope": context["frame"]["envelope"],
        "owner": owner.raw,
        "observer": observer,
        "anonymous_pause": anonymous_pause,
        "observer_pause": observer_pause,
        "owner_pause": owner_pause,
        "owner_resume": owner_resume,
        "owner_dispatch": owner_dispatch,
        "observer_status": observer_status,
    },
}
proof["ok"] = all(value for key, value in proof.items() if key not in {"schema_version", "artifacts"})
with open(proof_path, "w", encoding="utf-8") as handle:
    json.dump(proof, handle, indent=2)
    handle.write("\n")
print(json.dumps(proof, indent=2))
if not proof["ok"]:
    raise SystemExit(1)
PY

jq -e '.ok == true' "$PROOF" >/dev/null
