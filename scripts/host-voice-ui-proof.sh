#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v screencapture >/dev/null
command -v python3 >/dev/null

RUN_ID="$(date +%s)"
PROFILE="${CUA_VOICE_UI_PROOF_PROFILE:-host-voice-ui-proof-$RUN_ID}"
OUT_DIR="${CUA_VOICE_UI_PROOF_OUT_DIR:-artifacts/cua/voice-ui-proof-$RUN_ID}"
BEFORE="$OUT_DIR/before.png"
COMPACT="$OUT_DIR/compact.png"
REPLY="$OUT_DIR/reply.png"
COLLAPSED="$OUT_DIR/collapsed.png"
PROOF="$OUT_DIR/proof.json"

cargo build -p cua-voice

if [[ -n "${CUA_VOICE_BIN:-}" ]]; then
  BIN="$CUA_VOICE_BIN"
elif [[ -x target/debug/cua-voice ]]; then
  BIN="target/debug/cua-voice"
else
  BIN="$(find target -path '*/debug/cua-voice' -type f 2>/dev/null | head -n 1)"
fi

if [[ -z "$BIN" || ! -x "$BIN" ]]; then
  echo "cua-voice binary not found" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"
screencapture -x "$BEFORE"

OPENROUTER_API_KEY="${OPENROUTER_API_KEY:-ui-proof-only}" "$BIN" \
  --profile "$PROFILE" \
  --demo &
PID="$!"

cleanup() {
  kill "$PID" >/dev/null 2>&1 || true
  wait "$PID" >/dev/null 2>&1 || true
}
trap cleanup EXIT

sleep "${CUA_VOICE_UI_PROOF_COMPACT_WAIT_SECS:-2}"
if ! kill -0 "$PID" >/dev/null 2>&1; then
  echo "cua-voice exited before visual proof capture" >&2
  exit 1
fi

screencapture -x "$COMPACT"
sleep "${CUA_VOICE_UI_PROOF_REPLY_WAIT_SECS:-4}"
screencapture -x "$REPLY"
sleep "${CUA_VOICE_UI_PROOF_COLLAPSED_WAIT_SECS:-7}"
screencapture -x "$COLLAPSED"

python3 - "$BEFORE" "$COMPACT" "$REPLY" "$COLLAPSED" "$PROOF" <<'PY'
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


def region_metrics(base, base_channels, target, target_channels, x0, y0, width, height):
    changed = 0
    dark_target = 0
    darkened = 0
    total = width * height
    for y in range(y0, y0 + height):
        for x in range(x0, x0 + width):
            br, bg, bb, _ = pixel(base, base_channels, x, y)
            ar, ag, ab, _ = pixel(target, target_channels, x, y)
            delta = abs(ar - br) + abs(ag - bg) + abs(ab - bb)
            before_luma = (br * 299 + bg * 587 + bb * 114) / 1000
            after_luma = (ar * 299 + ag * 587 + ab * 114) / 1000
            if delta >= 18:
                changed += 1
            if after_luma <= 42:
                dark_target += 1
            if before_luma - after_luma >= 18:
                darkened += 1
    return {
        "x": x0,
        "y": y0,
        "width": width,
        "height": height,
        "changed_ratio": changed / total,
        "dark_ratio": dark_target / total,
        "darkened_ratio": darkened / total,
    }


before_path, compact_path, reply_path, collapsed_path, proof_path = sys.argv[1:6]
bw, bh, bc, before = read_png(before_path)
cw, ch, cc, compact = read_png(compact_path)
rw, rh, rc, reply = read_png(reply_path)
lw, lh, lc, collapsed = read_png(collapsed_path)
if len({(bw, bh), (cw, ch), (rw, rh), (lw, lh)}) != 1:
    raise SystemExit("visual proof screenshots have different dimensions")

top_width = min(920, cw)
top_height = min(96, ch)
top_x = max((cw - top_width) // 2, 0)
top_y = 0
compact_top = region_metrics(before, bc, compact, cc, top_x, top_y, top_width, top_height)
reply_top = region_metrics(before, bc, reply, rc, top_x, top_y, top_width, top_height)
collapsed_top = region_metrics(before, bc, collapsed, lc, top_x, top_y, top_width, top_height)

compact_ok = (
    compact_top["changed_ratio"] >= 0.0025
    and compact_top["dark_ratio"] >= 0.05
    and compact_top["darkened_ratio"] >= 0.001
)
reply_ok = (
    reply_top["changed_ratio"] >= 0.0025
    and reply_top["dark_ratio"] >= 0.05
    and reply_top["darkened_ratio"] >= 0.001
)
collapsed_ok = collapsed_top["darkened_ratio"] <= compact_top["darkened_ratio"] + 0.002
ok = compact_ok and reply_ok and collapsed_ok
proof = {
    "schema_version": "cua.voice_ui_proof.v1",
    "screen": {"width": cw, "height": ch},
    "before": before_path,
    "compact": {"path": compact_path, "top": compact_top, "ok": compact_ok},
    "reply": {
        "path": reply_path,
        "top": reply_top,
        "ok": reply_ok,
    },
    "collapsed": {"path": collapsed_path, "top": collapsed_top, "ok": collapsed_ok},
    "ok": ok,
}
with open(proof_path, "w", encoding="utf-8") as handle:
    json.dump(proof, handle, indent=2)
    handle.write("\n")
if not ok:
    raise SystemExit(json.dumps(proof, indent=2))
PY

echo "$OUT_DIR"
