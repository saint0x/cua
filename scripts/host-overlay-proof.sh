#!/usr/bin/env bash
set -euo pipefail

export CUA_DEV_HTTP_TOKEN_OVERRIDE=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v python3 >/dev/null

if [[ -n "${CUA_BIN:-}" ]]; then
  CUA_BIN_PATH="$CUA_BIN"
elif [[ -x target/debug/cua ]]; then
  CUA_BIN_PATH="target/debug/cua"
else
  CUA_BIN_PATH="$(find target -path '*/debug/cua' -type f 2>/dev/null | head -n 1)"
fi

if [[ -n "${CUA_VOICE_BIN:-}" ]]; then
  CUA_VOICE_BIN_PATH="$CUA_VOICE_BIN"
elif [[ -x target/debug/cua-voice ]]; then
  CUA_VOICE_BIN_PATH="target/debug/cua-voice"
else
  CUA_VOICE_BIN_PATH="$(find target -path '*/debug/cua-voice' -type f 2>/dev/null | head -n 1)"
fi

if [[ -z "$CUA_BIN_PATH" || ! -x "$CUA_BIN_PATH" ]]; then
  cargo build -p cua
fi
if [[ -z "$CUA_BIN_PATH" || ! -x "$CUA_BIN_PATH" ]]; then
  if [[ -x target/debug/cua ]]; then
    CUA_BIN_PATH="target/debug/cua"
  else
    CUA_BIN_PATH="$(find target -path '*/debug/cua' -type f 2>/dev/null | head -n 1)"
  fi
fi
if [[ -z "$CUA_BIN_PATH" || ! -x "$CUA_BIN_PATH" ]]; then
  echo "cua binary not found" >&2
  exit 1
fi
if [[ -z "$CUA_VOICE_BIN_PATH" || ! -x "$CUA_VOICE_BIN_PATH" ]]; then
  cargo build -p cua-voice
fi
if [[ -z "$CUA_VOICE_BIN_PATH" || ! -x "$CUA_VOICE_BIN_PATH" ]]; then
  if [[ -x target/debug/cua-voice ]]; then
    CUA_VOICE_BIN_PATH="target/debug/cua-voice"
  else
    CUA_VOICE_BIN_PATH="$(find target -path '*/debug/cua-voice' -type f 2>/dev/null | head -n 1)"
  fi
fi
if [[ -z "$CUA_VOICE_BIN_PATH" || ! -x "$CUA_VOICE_BIN_PATH" ]]; then
  echo "cua-voice binary not found" >&2
  exit 1
fi

RUN_ID="$(date +%s)"
PROFILE="${CUA_OVERLAY_PROOF_PROFILE:-overlay-proof-$RUN_ID}"
ADDR="${CUA_OVERLAY_PROOF_ADDR:-127.0.0.1:$((33000 + RUN_ID % 1000))}"
TOKEN="${CUA_OVERLAY_PROOF_TOKEN:-overlay-proof-token-$RUN_ID}"
OUT_DIR="${CUA_OVERLAY_PROOF_OUT_DIR:-artifacts/cua/overlay-proof-$RUN_ID}"
SOCKET="$HOME/.cua/profiles/$PROFILE/daemon.sock"
OBSERVE_JSON="$OUT_DIR/observe.json"
CAPTURE_JSON="$OUT_DIR/window-capture.json"
CAPTURE_PNG="$OUT_DIR/window-capture.png"
PROOF_JSON="$OUT_DIR/proof.json"
mkdir -p "$OUT_DIR"

CUA_HTTP_TOKEN="$TOKEN" CUA_HUD_AUTOSTART=0 "$CUA_BIN_PATH" --profile "$PROFILE" \
  serve --addr "$ADDR" --hud-mode headless >"$OUT_DIR/daemon.log" 2>&1 &
DAEMON_PID="$!"
VOICE_PID=""

cleanup() {
  if [[ -n "$VOICE_PID" ]]; then
    kill "$VOICE_PID" >/dev/null 2>&1 || true
    wait "$VOICE_PID" >/dev/null 2>&1 || true
  fi
  kill "$DAEMON_PID" >/dev/null 2>&1 || true
  wait "$DAEMON_PID" >/dev/null 2>&1 || true
}
trap cleanup EXIT

for _ in $(seq 1 100); do
  if [[ -S "$SOCKET" ]] && CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" \
    --profile "$PROFILE" --server-addr "$ADDR" status --json >/dev/null 2>&1; then
    break
  fi
  sleep 0.05
done
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --profile "$PROFILE" \
  --server-addr "$ADDR" status --json >"$OUT_DIR/status.json"

OPENROUTER_API_KEY="${OPENROUTER_API_KEY:-ui-proof-only}" \
CUA_HTTP_TOKEN="$TOKEN" \
"$CUA_VOICE_BIN_PATH" --profile "$PROFILE" --demo >"$OUT_DIR/voice.log" 2>&1 &
VOICE_PID="$!"

sleep "${CUA_OVERLAY_PROOF_WAIT_SECS:-2}"
if ! kill -0 "$VOICE_PID" >/dev/null 2>&1; then
  echo "cua-voice exited before overlay proof" >&2
  exit 1
fi

CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --profile "$PROFILE" \
  --server-addr "$ADDR" observe --json >"$OBSERVE_JSON"

WINDOW_ID="$(
  python3 - "$OBSERVE_JSON" "$PROOF_JSON.candidate" <<'PY'
import json
import sys

observe_path, candidate_path = sys.argv[1:3]
obs = json.load(open(observe_path))
windows = [
    w for w in obs.get("windows", [])
    if (w.get("app_name") or "").lower() == "cua"
]
windows.sort(
    key=lambda w: (
        abs(int(w.get("width", 0)) - 816) + abs(int(w.get("height", 0)) - 42),
        -int(w.get("layer", 0)),
    )
)
if not windows:
    raise SystemExit("no cua window found")
candidate = windows[0]
json.dump(candidate, open(candidate_path, "w"), indent=2)
print(candidate["id"])
PY
)"

CUA_WINDOW_CAPTURE_TIMEOUT_MS="${CUA_WINDOW_CAPTURE_TIMEOUT_MS:-2500}" \
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --profile "$PROFILE" \
  --server-addr "$ADDR" window-capture "$WINDOW_ID" \
  --out "$CAPTURE_PNG" --max-width 1280 --json >"$CAPTURE_JSON"

python3 - "$OBSERVE_JSON" "$PROOF_JSON.candidate" "$CAPTURE_JSON" "$CAPTURE_PNG" "$PROOF_JSON" <<'PY'
import json
import os
import struct
import sys

observe_path, candidate_path, capture_json_path, png_path, proof_path = sys.argv[1:6]
candidate = json.load(open(candidate_path))
capture = json.load(open(capture_json_path))
envelope = capture

with open(png_path, "rb") as handle:
    png = handle.read()
if not png.startswith(b"\x89PNG\r\n\x1a\n"):
    raise SystemExit("window capture is not a PNG")
png_width, png_height = struct.unpack(">II", png[16:24])
if png_width <= 0 or png_height <= 0:
    raise SystemExit("window capture has empty dimensions")

if len(png) != envelope["byte_len"]:
    raise SystemExit("capture envelope byte_len and written PNG diverged")

width = int(candidate["width"])
height = int(candidate["height"])
layer = int(candidate.get("layer", 0))
x = int(candidate["x"])
y = int(candidate["y"])
frame_x = int(envelope["frame_origin_x"])
frame_y = int(envelope["frame_origin_y"])
origin_ok = abs(frame_x - x) <= 2 and abs(frame_y - y) <= 2
shape_ok = 500 <= width <= 1200 and 30 <= height <= 140 and layer >= 1
capture_shape_ok = abs(int(envelope["width"]) - png_width) <= 1 and abs(int(envelope["height"]) - png_height) <= 1
png_nontrivial = os.path.getsize(png_path) > 1024

proof = {
    "schema_version": "cua.overlay_proof.v1",
    "ok": bool(origin_ok and shape_ok and capture_shape_ok and png_nontrivial),
    "window": candidate,
    "origin_ok": origin_ok,
    "shape_ok": shape_ok,
    "capture_shape_ok": capture_shape_ok,
    "png_nontrivial": png_nontrivial,
    "capture": {
        "path": png_path,
        "bytes": os.path.getsize(png_path),
        "envelope": envelope,
    },
    "observed_windows": len(json.load(open(observe_path)).get("windows", [])),
}
json.dump(proof, open(proof_path, "w"), indent=2)
if not proof["ok"]:
    raise SystemExit(json.dumps(proof, indent=2))
PY

echo "$OUT_DIR"
