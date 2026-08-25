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
AFTER="$OUT_DIR/after.png"
PROOF="$OUT_DIR/proof.json"

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

sleep "${CUA_VOICE_UI_PROOF_WAIT_SECS:-2}"
if ! kill -0 "$PID" >/dev/null 2>&1; then
  echo "cua-voice exited before visual proof capture" >&2
  exit 1
fi

screencapture -x "$AFTER"

python3 - "$BEFORE" "$AFTER" "$PROOF" <<'PY'
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


before_path, after_path, proof_path = sys.argv[1:4]
bw, bh, bc, before = read_png(before_path)
aw, ah, ac, after = read_png(after_path)
if (bw, bh) != (aw, ah):
    raise SystemExit("before/after screenshots have different dimensions")

region_width = min(920, aw)
region_height = min(96, ah)
x0 = max((aw - region_width) // 2, 0)
y0 = 0
changed = 0
dark_after = 0
darkened = 0
total = region_width * region_height
for y in range(y0, y0 + region_height):
    for x in range(x0, x0 + region_width):
        br, bg, bb, _ = pixel(before, bc, x, y)
        ar, ag, ab, _ = pixel(after, ac, x, y)
        delta = abs(ar - br) + abs(ag - bg) + abs(ab - bb)
        before_luma = (br * 299 + bg * 587 + bb * 114) / 1000
        after_luma = (ar * 299 + ag * 587 + ab * 114) / 1000
        if delta >= 18:
            changed += 1
        if after_luma <= 42:
            dark_after += 1
        if before_luma - after_luma >= 18:
            darkened += 1

changed_ratio = changed / total
dark_ratio = dark_after / total
darkened_ratio = darkened / total
ok = changed_ratio >= 0.0025 and dark_ratio >= 0.05 and darkened_ratio >= 0.001
proof = {
    "schema_version": "cua.voice_ui_proof.v1",
    "screen": {"width": aw, "height": ah},
    "region": {"x": x0, "y": y0, "width": region_width, "height": region_height},
    "changed_ratio": changed_ratio,
    "dark_ratio": dark_ratio,
    "darkened_ratio": darkened_ratio,
    "before": before_path,
    "after": after_path,
    "ok": ok,
}
with open(proof_path, "w", encoding="utf-8") as handle:
    json.dump(proof, handle, indent=2)
    handle.write("\n")
if not ok:
    raise SystemExit(json.dumps(proof, indent=2))
PY

echo "$OUT_DIR"
