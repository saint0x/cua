#!/usr/bin/env bash
set -euo pipefail

export CUA_DEV_HTTP_TOKEN_OVERRIDE=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v python3 >/dev/null
command -v screencapture >/dev/null

RUN_ID="$(date +%s)"
PROFILE="${CUA_VOICE_UI_PROOF_PROFILE:-default}"
OUT_DIR="${CUA_VOICE_UI_PROOF_OUT_DIR:-artifacts/cua/voice-ui-proof-$RUN_ID}"
if [[ -z "${CUA_HTTP_TOKEN:-}" && "$PROFILE" != "default" ]]; then
  export CUA_HTTP_TOKEN="cua-ui-proof-$RUN_ID"
fi
COMPACT="$OUT_DIR/compact.png"
EXPANDED="$OUT_DIR/expanded.png"
REPLY="$OUT_DIR/reply.png"
COLLAPSED="$OUT_DIR/collapsed.png"
PROOF="$OUT_DIR/proof.json"
STATUS_INITIAL="$OUT_DIR/status-initial.json"
STATUS_FINAL="$OUT_DIR/status-final.json"

cargo build -p cua -p cua-voice

if [[ -n "${CUA_BIN:-}" ]]; then
  CUA_BIN_PATH="$CUA_BIN"
elif [[ -x target/debug/cua ]]; then
  CUA_BIN_PATH="target/debug/cua"
else
  CUA_BIN_PATH="$(find target -path '*/debug/cua' -type f 2>/dev/null | head -n 1)"
fi

if [[ -n "${CUA_VOICE_BIN:-}" ]]; then
  BIN="$CUA_VOICE_BIN"
elif [[ -x target/debug/cua-voice ]]; then
  BIN="target/debug/cua-voice"
else
  BIN="$(find target -path '*/debug/cua-voice' -type f 2>/dev/null | head -n 1)"
fi

if [[ -z "$CUA_BIN_PATH" || ! -x "$CUA_BIN_PATH" ]]; then
  echo "cua binary not found" >&2
  exit 1
fi
if [[ -z "$BIN" || ! -x "$BIN" ]]; then
  echo "cua-voice binary not found" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"

capture_hud_window_png() {
  local label="$1"
  local out="$2"
  local observe="$OUT_DIR/$label-observe.json"
  local selected="$OUT_DIR/$label-window.json"

  "$CUA_BIN_PATH" --profile "$PROFILE" observe --json >"$observe"
  local window_id
  window_id="$(
    python3 - "$observe" "$selected" <<'PY'
import json
import sys

observe_path, selected_path = sys.argv[1:3]
observe = json.load(open(observe_path, encoding="utf-8"))
windows = [
    window for window in observe.get("windows", [])
    if (window.get("app_name") or "").lower() in {"cua", "cua-voice"}
    and int(window.get("layer") or 0) >= 20
    and int(window.get("width") or 0) >= 300
    and int(window.get("height") or 0) >= 30
]
windows.sort(
    key=lambda window: (
        -int(window.get("layer") or 0),
        -int(window.get("width") or 0) * int(window.get("height") or 0),
    )
)
if not windows:
    raise SystemExit("no visible cua HUD window found")
selected = windows[0]
json.dump(selected, open(selected_path, "w", encoding="utf-8"), indent=2)
print(selected["id"])
PY
  )"
  screencapture -x -l "$window_id" "$out"
}

"$BIN" \
  --profile "$PROFILE" \
  --headful >"$OUT_DIR/voice.log" 2>&1 &
PID="$!"

cleanup() {
  kill "$PID" >/dev/null 2>&1 || true
  wait "$PID" >/dev/null 2>&1 || true
}
trap cleanup EXIT

sleep "${CUA_VOICE_UI_PROOF_COMPACT_WAIT_SECS:-2}"
for _ in $(seq 1 100); do
  if "$CUA_BIN_PATH" --profile "$PROFILE" status --json >/dev/null 2>&1; then
    break
  fi
  sleep 0.05
done
"$CUA_BIN_PATH" --profile "$PROFILE" status --json >"$STATUS_INITIAL"

"$CUA_BIN_PATH" --profile "$PROFILE" ui step "Checking desktop context" \
  --source host-voice-ui-proof \
  --task "HUD visual proof" \
  --tool "Unix socket" \
  --step-index 1 \
  --step-total 4 \
  --json >/dev/null
capture_hud_window_png compact "$COMPACT"

"$CUA_BIN_PATH" --profile "$PROFILE" \
  ui island expanded --source host-voice-ui-proof --json >/dev/null
sleep "${CUA_VOICE_UI_PROOF_EXPANDED_WAIT_SECS:-1}"
capture_hud_window_png expanded "$EXPANDED"

"$CUA_BIN_PATH" --profile "$PROFILE" \
  ui island collapsed --source host-voice-ui-proof --json >/dev/null
"$CUA_BIN_PATH" --profile "$PROFILE" ui reply "HUD proof accepted" \
  --source host-voice-ui-proof \
  --json >/dev/null
sleep "${CUA_VOICE_UI_PROOF_REPLY_WAIT_SECS:-4}"
capture_hud_window_png reply "$REPLY"

sleep "${CUA_VOICE_UI_PROOF_COLLAPSED_WAIT_SECS:-9}"
capture_hud_window_png collapsed "$COLLAPSED"
"$CUA_BIN_PATH" --profile "$PROFILE" status --json >"$STATUS_FINAL" || true

python3 - "$OUT_DIR" "$COMPACT" "$EXPANDED" "$REPLY" "$COLLAPSED" "$PROOF" <<'PY'
import json
import struct
import sys
import zlib


def read_png(path):
    with open(path, "rb") as handle:
        data = handle.read()
    if not data.startswith(b"\x89PNG\r\n\x1a\n"):
        raise SystemExit(f"{path} is not a PNG")
    offset = 8
    width = height = color_type = None
    payload = bytearray()
    while offset < len(data):
        length = struct.unpack(">I", data[offset : offset + 4])[0]
        kind = data[offset + 4 : offset + 8]
        chunk = data[offset + 8 : offset + 8 + length]
        offset += 12 + length
        if kind == b"IHDR":
            width, height, bit_depth, color_type = struct.unpack(">IIBB", chunk[:10])
            if bit_depth != 8 or color_type not in (2, 6):
                raise SystemExit(f"unsupported PNG format bit_depth={bit_depth} color_type={color_type}")
        elif kind == b"IDAT":
            payload.extend(chunk)
        elif kind == b"IEND":
            break
    channels = 4 if color_type == 6 else 3
    stride = width * channels
    raw = zlib.decompress(bytes(payload))
    rows = []
    previous = [0] * stride
    cursor = 0
    for _ in range(height):
        filter_type = raw[cursor]
        cursor += 1
        row = list(raw[cursor : cursor + stride])
        cursor += stride
        for i, value in enumerate(row):
            left = row[i - channels] if i >= channels else 0
            up = previous[i]
            up_left = previous[i - channels] if i >= channels else 0
            if filter_type == 1:
                row[i] = (value + left) & 255
            elif filter_type == 2:
                row[i] = (value + up) & 255
            elif filter_type == 3:
                row[i] = (value + ((left + up) // 2)) & 255
            elif filter_type == 4:
                p = left + up - up_left
                pa = abs(p - left)
                pb = abs(p - up)
                pc = abs(p - up_left)
                predictor = left if pa <= pb and pa <= pc else up if pb <= pc else up_left
                row[i] = (value + predictor) & 255
            elif filter_type != 0:
                raise SystemExit(f"unsupported PNG filter {filter_type}")
        rows.append(row)
        previous = row
    return width, height, channels, rows


def pixel(rows, channels, x, y):
    row = rows[y]
    index = x * channels
    r, g, b = row[index], row[index + 1], row[index + 2]
    a = row[index + 3] if channels == 4 else 255
    return r, g, b, a


def bbox_for(predicate, rows, channels):
    width = len(rows[0]) // channels
    height = len(rows)
    count = 0
    min_x = min_y = 10**9
    max_x = max_y = -1
    for y in range(height):
        for x in range(width):
            if not predicate(*pixel(rows, channels, x, y)):
                continue
            count += 1
            min_x = min(min_x, x)
            min_y = min(min_y, y)
            max_x = max(max_x, x)
            max_y = max(max_y, y)
    bbox = None
    if count:
        bbox = {
            "x": min_x,
            "y": min_y,
            "width": max_x - min_x + 1,
            "height": max_y - min_y + 1,
        }
    return {"count": count, "bbox": bbox, "ratio": count / (width * height)}


def capture_metrics(path):
    width, height, channels, rows = read_png(path)
    return {
        "path": path,
        "width": width,
        "height": height,
        "alpha": bbox_for(lambda _r, _g, _b, a: a > 8, rows, channels),
        "dark": bbox_for(
            lambda r, g, b, a: a > 128 and (r * 299 + g * 587 + b * 114) / 1000 < 30,
            rows,
            channels,
        ),
        "bright": bbox_for(
            lambda r, g, b, a: a > 64 and (r * 299 + g * 587 + b * 114) / 1000 > 80,
            rows,
            channels,
        ),
    }


def compact_like_ok(metrics):
    dark = metrics["dark"]["bbox"]
    if dark is None:
        return False
    center_error = abs((dark["x"] + dark["width"] / 2) - metrics["width"] / 2)
    return (
        1000 <= dark["width"] <= 1800
        and 55 <= dark["height"] <= 135
        and dark["y"] <= 95
        and center_error <= 140
        and metrics["bright"]["count"] >= 1000
    )


def expanded_like_ok(metrics):
    dark = metrics["dark"]["bbox"]
    if dark is None:
        return False
    center_error = abs((dark["x"] + dark["width"] / 2) - metrics["width"] / 2)
    return (
        1000 <= dark["width"] <= 1900
        and 450 <= dark["height"] <= 1300
        and dark["y"] <= 140
        and center_error <= 180
        and metrics["bright"]["count"] >= 2500
    )


def load_window(out_dir, label):
    return json.load(open(f"{out_dir}/{label}-window.json", encoding="utf-8"))


out_dir, compact_path, expanded_path, reply_path, collapsed_path, proof_path = sys.argv[1:7]
compact = capture_metrics(compact_path)
expanded = capture_metrics(expanded_path)
reply = capture_metrics(reply_path)
collapsed = capture_metrics(collapsed_path)

proof = {
    "schema_version": "cua.voice_ui_proof.v1",
    "capture": "native_window",
    "compact": {
        "metrics": compact,
        "window": load_window(out_dir, "compact"),
        "ok": compact_like_ok(compact),
    },
    "expanded": {
        "metrics": expanded,
        "window": load_window(out_dir, "expanded"),
        "ok": expanded_like_ok(expanded),
    },
    "reply": {
        "metrics": reply,
        "window": load_window(out_dir, "reply"),
        "ok": compact_like_ok(reply),
    },
    "collapsed": {
        "metrics": collapsed,
        "window": load_window(out_dir, "collapsed"),
        "ok": compact_like_ok(collapsed),
    },
}
proof["ok"] = all(section["ok"] for section in [
    proof["compact"],
    proof["expanded"],
    proof["reply"],
    proof["collapsed"],
])

with open(proof_path, "w", encoding="utf-8") as handle:
    json.dump(proof, handle, indent=2)
    handle.write("\n")
if not proof["ok"]:
    raise SystemExit(json.dumps(proof, indent=2))
PY

echo "$OUT_DIR"
