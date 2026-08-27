#!/usr/bin/env bash
set -euo pipefail

export CUA_DEV_HTTP_TOKEN_OVERRIDE=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v curl >/dev/null
command -v jq >/dev/null
command -v python3 >/dev/null

debug_bin() {
  local name="$1"
  local target_root="${CARGO_TARGET_DIR:-target}"
  if [[ -x "$target_root/debug/$name" ]]; then
    printf '%s\n' "$target_root/debug/$name"
  elif [[ -x "target/debug/$name" ]]; then
    printf '%s\n' "target/debug/$name"
  else
    find "$target_root" target -path "*/debug/$name" -type f -perm -111 2>/dev/null | head -n 1 || true
  fi
}

RUN_ID="$(date +%s)"
PROFILE="${CUA_SCRATCHPAD_PROOF_PROFILE:-host-scratchpad-proof-$RUN_ID}"
ADDR="${CUA_SCRATCHPAD_PROOF_ADDR:-127.0.0.1:$((34000 + RUN_ID % 1000))}"
TOKEN="${CUA_HTTP_TOKEN:-host-scratchpad-proof-token-$RUN_ID}"
OUT_DIR="${CUA_SCRATCHPAD_PROOF_OUT_DIR:-artifacts/cua/scratchpad-proof-$RUN_ID}"
CUA_HOME_DIR="${CUA_SCRATCHPAD_PROOF_CUA_HOME:-/tmp/cua-sp-$RUN_ID}"
SOCKET="$CUA_HOME_DIR/profiles/$PROFILE/daemon.sock"
PROOF="$OUT_DIR/proof.json"
OWNER="scratchpad-proof-owner"
mkdir -p "$OUT_DIR"

CUA_BIN_PATH="${CUA_BIN:-$(debug_bin cua)}"
if [[ -z "$CUA_BIN_PATH" || ! -x "$CUA_BIN_PATH" ]]; then
  cargo build -p cua >/dev/null
  CUA_BIN_PATH="${CUA_BIN:-$(debug_bin cua)}"
fi
if [[ -z "$CUA_BIN_PATH" || ! -x "$CUA_BIN_PATH" ]]; then
  echo "cua binary not found" >&2
  exit 1
fi

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
  if [[ -S "$SOCKET" ]] && curl -fs "http://$ADDR/healthz" >/dev/null 2>&1; then
    break
  fi
  sleep 0.05
done
curl -fsS "http://$ADDR/healthz" >/dev/null

CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --profile "$PROFILE" \
  session acquire "$OWNER" --role owner --client-name scratchpad-proof --ttl-ms 60000 --json \
  > "$OUT_DIR/session.json"

CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --profile "$PROFILE" \
  scratchpad write cli-note "CLI durable note" --session-id "$OWNER" --json \
  > "$OUT_DIR/cli-write.json"

CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --profile "$PROFILE" \
  scratchpad write cli-note "appended line" --session-id "$OWNER" --append --json \
  > "$OUT_DIR/cli-append.json"

CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --profile "$PROFILE" \
  scratchpad read cli-note --json > "$OUT_DIR/cli-read.json"

curl -fsS \
  -H "authorization: Bearer $TOKEN" \
  -H "content-type: application/json" \
  -H "x-cua-session-id: $OWNER" \
  -d '{"schema_version":"cua.v1","name":"http-note","text":"HTTP durable note","durable":true}' \
  "http://$ADDR/scratchpads/write" > "$OUT_DIR/http-write.json"

curl -fsS \
  -H "authorization: Bearer $TOKEN" \
  -H "content-type: application/json" \
  -d '{"schema_version":"cua.v1","include_durable":true,"include_ephemeral":true}' \
  "http://$ADDR/scratchpads/list" > "$OUT_DIR/http-list.json"

python3 - "$SOCKET" "$TOKEN" > "$OUT_DIR/unix.json" <<'PY'
import json
import socket
import sys
import uuid

path, token = sys.argv[1:3]

def call(method, params, session_id=None):
    request = {
        "id": str(uuid.uuid4()),
        "token": token,
        "method": method,
        "params": params,
    }
    if session_id:
        request["session_id"] = session_id
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.connect(path)
    client.sendall((json.dumps(request) + "\n").encode("utf-8"))
    line = b""
    while not line.endswith(b"\n"):
        chunk = client.recv(65536)
        if not chunk:
            break
        line += chunk
    response = json.loads(line.decode("utf-8"))
    if not response.get("ok"):
        raise RuntimeError(response)
    return response["result"]

print(json.dumps({
    "read": call("scratchpad.read", {"schema_version": "cua.v1", "name": "cli-note"}),
    "list": call("scratchpad.list", {"schema_version": "cua.v1"}),
}, indent=2))
PY

curl -fsS \
  -H "authorization: Bearer $TOKEN" \
  -H "content-type: application/json" \
  -H "x-cua-session-id: $OWNER" \
  -d '{"schema_version":"cua.v1","name":"http-note","durable":true,"ephemeral":true}' \
  "http://$ADDR/scratchpads/delete" > "$OUT_DIR/http-delete.json"

python3 - "$OUT_DIR" "$CUA_HOME_DIR" "$PROFILE" "$PROOF" <<'PY'
import json
import sys
from pathlib import Path

out_dir = Path(sys.argv[1])
cua_home = Path(sys.argv[2]).resolve()
profile = sys.argv[3]
proof_path = Path(sys.argv[4])

def load(name):
    return json.loads((out_dir / name).read_text())

scratch_root = (cua_home / "profiles" / profile / "scratchpads").resolve()
cli_file = scratch_root / "durable" / "cli-note.json"
http_file = scratch_root / "durable" / "http-note.json"
cli_read = load("cli-read.json")
http_list = load("http-list.json")
unix = load("unix.json")
http_delete = load("http-delete.json")

proof = {
    "schema_version": "cua.scratchpad_proof.v1",
    "profile": profile,
    "cli_round_trip": cli_read["profile"] == profile
    and cli_read["name"] == "cli-note"
    and "CLI durable note" in cli_read["text"]
    and "appended line" in cli_read["text"],
    "unix_round_trip": unix["read"]["name"] == "cli-note"
    and unix["list"]["entries"][0]["profile"] == profile,
    "http_round_trip": any(entry["name"] == "http-note" for entry in http_list["entries"]),
    "http_delete": http_delete["deleted"] == 1 and not http_file.exists(),
    "profile_scoped_path": cli_file.exists()
    and str(cli_file.resolve()).startswith(str(scratch_root)),
    "repo_path_clean": not str(cli_file.resolve()).startswith(str(Path.cwd().resolve())),
    "scratch_root": str(scratch_root),
}
proof["ok"] = all(value for key, value in proof.items() if key not in {"schema_version", "profile", "scratch_root"})
proof_path.write_text(json.dumps(proof, indent=2) + "\n")
print(json.dumps(proof, indent=2))
if not proof["ok"]:
    raise SystemExit(1)
PY

jq -e '.ok == true' "$PROOF" >/dev/null
