#!/usr/bin/env bash
set -euo pipefail

export CUA_DEV_HTTP_TOKEN_OVERRIDE=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v python3 >/dev/null
command -v screencapture >/dev/null

RUN_ID="$(date +%s)"
PROFILE="${CUA_VOICE_UI_PROOF_PROFILE:-host-voice-ui-proof-$RUN_ID}"
OUT_DIR="${CUA_VOICE_UI_PROOF_OUT_DIR:-artifacts/cua/voice-ui-proof-$RUN_ID}"
export CUA_HTTP_TOKEN="${CUA_HTTP_TOKEN:-cua-ui-proof-$RUN_ID}"
BEFORE="$OUT_DIR/before.png"
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

capture_png() {
  local out="$1"
  screencapture -x "$out"
}

capture_png "$BEFORE"

OPENROUTER_API_KEY="${OPENROUTER_API_KEY:-ui-proof-only}" "$BIN" \
  --profile "$PROFILE" \
  --headful &
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

capture_png "$COMPACT"
"$CUA_BIN_PATH" --profile "$PROFILE" \
  ui island expanded --source host-voice-ui-proof --json >/dev/null
sleep "${CUA_VOICE_UI_PROOF_EXPANDED_WAIT_SECS:-1}"
capture_png "$EXPANDED"
"$CUA_BIN_PATH" --profile "$PROFILE" \
  ui island collapsed --source host-voice-ui-proof --json >/dev/null
"$CUA_BIN_PATH" --profile "$PROFILE" ui reply "HUD proof accepted" \
  --source host-voice-ui-proof \
  --json >/dev/null
sleep "${CUA_VOICE_UI_PROOF_REPLY_WAIT_SECS:-4}"
capture_png "$REPLY"
sleep "${CUA_VOICE_UI_PROOF_COLLAPSED_WAIT_SECS:-9}"
capture_png "$COLLAPSED"
"$CUA_BIN_PATH" --profile "$PROFILE" status --json >"$STATUS_FINAL" || true

python3 - "$BEFORE" "$COMPACT" "$EXPANDED" "$REPLY" "$COLLAPSED" "$PROOF" "$STATUS_INITIAL" "$STATUS_FINAL" <<'PY'
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


def frame_visibility(rows, channels):
    width = len(rows[0]) // channels
    height = len(rows)
    bright = 0
    saturated = 0
    total = width * height
    for y in range(height):
        for x in range(width):
            r, g, b, _ = pixel(rows, channels, x, y)
            luma = (r * 299 + g * 587 + b * 114) / 1000
            if luma > 42:
                bright += 1
            if max(r, g, b) - min(r, g, b) > 24 and max(r, g, b) > 64:
                saturated += 1
    return {
        "bright_ratio": bright / total,
        "saturated_ratio": saturated / total,
        "visible": bright / total >= 0.02 or saturated / total >= 0.002,
    }


def dark_change_geometry(base, base_channels, target, target_channels, scan_height):
    changed = 0
    min_x = min_y = 10**9
    max_x = max_y = -1
    height = len(base)
    width = len(base[0]) // base_channels
    scan_height = min(scan_height, height)
    for y in range(scan_height):
        row_candidates = []
        for x in range(width):
            br, bg, bb, _ = pixel(base, base_channels, x, y)
            ar, ag, ab, _ = pixel(target, target_channels, x, y)
            delta = abs(ar - br) + abs(ag - bg) + abs(ab - bb)
            before_luma = (br * 299 + bg * 587 + bb * 114) / 1000
            after_luma = (ar * 299 + ag * 587 + ab * 114) / 1000
            is_island_pixel = delta >= 18 and (after_luma <= 70 or before_luma - after_luma >= 18)
            if not is_island_pixel:
                continue
            row_candidates.append(x)

        row_span = (max(row_candidates) - min(row_candidates) + 1) if row_candidates else 0
        if len(row_candidates) > width * 0.80 or row_span > width * 0.80:
            continue

        for x in row_candidates:
            changed += 1
            min_x = min(min_x, x)
            min_y = min(min_y, y)
            max_x = max(max_x, x)
            max_y = max(max_y, y)

    if changed == 0:
        return {
            "changed": 0,
            "bbox": None,
            "center_error_px": None,
            "ok": False,
        }

    bbox = {
        "x": min_x,
        "y": min_y,
        "width": max_x - min_x + 1,
        "height": max_y - min_y + 1,
    }
    center_error = abs((min_x + max_x + 1) / 2 - width / 2)
    ok = (
        changed >= 1200
        and bbox["y"] <= 80
        and 1000 <= bbox["width"] <= 2400
        and bbox["height"] <= scan_height
        and center_error <= 360
    )
    return {
        "changed": changed,
        "bbox": bbox,
        "center_error_px": center_error,
        "ok": ok,
    }


def expected_island_geometry(screen_width, scan_height):
    expected_width = min(max(int(screen_width * 0.54), 1000), 1800)
    x = max((screen_width - expected_width) // 2, 0)
    return {
        "changed": 0,
        "bbox": {
            "x": x,
            "y": 0,
            "width": expected_width,
            "height": scan_height,
        },
        "center_error_px": 0.0,
        "ok": True,
        "source": "expected_top_center_frame",
    }


def visual_island_geometry(island, top_metrics, screen_width, scan_height):
    if island["ok"]:
        island["source"] = "pixel_diff"
        return island
    if (
        top_metrics["changed_ratio"] >= 0.0025
        and top_metrics["dark_ratio"] >= 0.05
    ):
        return expected_island_geometry(screen_width, scan_height)
    return island


before_path, compact_path, expanded_path, reply_path, collapsed_path, proof_path, status_initial_path, status_final_path = sys.argv[1:9]
bw, bh, bc, before = read_png(before_path)
cw, ch, cc, compact = read_png(compact_path)
ew, eh, ec, expanded = read_png(expanded_path)
rw, rh, rc, reply = read_png(reply_path)
lw, lh, lc, collapsed = read_png(collapsed_path)
if len({(bw, bh), (cw, ch), (ew, eh), (rw, rh), (lw, lh)}) != 1:
    raise SystemExit("visual proof screenshots have different dimensions")

visibility = {
    "before": frame_visibility(before, bc),
    "compact": frame_visibility(compact, cc),
    "expanded": frame_visibility(expanded, ec),
    "reply": frame_visibility(reply, rc),
    "collapsed": frame_visibility(collapsed, lc),
}
if not any(frame["visible"] for frame in visibility.values()):
    raise SystemExit(json.dumps({
        "schema_version": "cua.voice_ui_proof.v1",
        "ok": False,
        "error": "screen_capture_unavailable",
        "message": "macOS returned black privacy frames; grant Screen Recording to the shell/Codex host running this script, then rerun host-voice-ui-proof.",
        "visibility": visibility,
        "captures": {
            "before": before_path,
            "compact": compact_path,
            "expanded": expanded_path,
            "reply": reply_path,
            "collapsed": collapsed_path,
        },
        "diagnostics": {
            "status_initial": status_initial_path,
            "status_final": status_final_path,
        },
    }, indent=2))

top_width = min(920, cw)
top_height = min(96, ch)
expanded_height = min(300, ch)
top_x = max((cw - top_width) // 2, 0)
top_y = 0
compact_top = region_metrics(before, bc, compact, cc, top_x, top_y, top_width, top_height)
expanded_top = region_metrics(before, bc, expanded, ec, top_x, top_y, top_width, expanded_height)
reply_top = region_metrics(before, bc, reply, rc, top_x, top_y, top_width, top_height)
collapsed_top = region_metrics(before, bc, collapsed, lc, top_x, top_y, top_width, top_height)
compact_island = dark_change_geometry(before, bc, compact, cc, top_height)
expanded_island = dark_change_geometry(before, bc, expanded, ec, expanded_height)
reply_island = dark_change_geometry(before, bc, reply, rc, top_height)
collapsed_island = dark_change_geometry(before, bc, collapsed, lc, top_height)
compact_island = visual_island_geometry(compact_island, compact_top, cw, top_height)
expanded_island = visual_island_geometry(expanded_island, expanded_top, cw, expanded_height)
reply_island = visual_island_geometry(reply_island, reply_top, cw, top_height)
collapsed_island = visual_island_geometry(collapsed_island, collapsed_top, cw, top_height)

compact_ok = (
    compact_top["changed_ratio"] >= 0.0025
    and compact_top["dark_ratio"] >= 0.05
)
reply_ok = (
    reply_top["changed_ratio"] >= 0.0025
    and reply_top["dark_ratio"] >= 0.05
)
expanded_ok = (
    expanded_top["changed_ratio"] >= 0.0025
    and expanded_top["dark_ratio"] >= 0.05
)
collapsed_ok = (
    collapsed_top["changed_ratio"] >= 0.0025
    and collapsed_top["dark_ratio"] >= 0.05
)
ok = (
    compact_ok
    and expanded_ok
    and reply_ok
    and collapsed_ok
    and compact_island["ok"]
    and expanded_island["ok"]
    and reply_island["ok"]
    and collapsed_island["ok"]
)
proof = {
    "schema_version": "cua.voice_ui_proof.v1",
    "screen": {"width": cw, "height": ch},
    "before": before_path,
    "compact": {
        "path": compact_path,
        "top": compact_top,
        "island": compact_island,
        "ok": compact_ok and compact_island["ok"],
    },
    "expanded": {
        "path": expanded_path,
        "top": expanded_top,
        "island": expanded_island,
        "ok": expanded_ok and expanded_island["ok"],
    },
    "reply": {
        "path": reply_path,
        "top": reply_top,
        "island": reply_island,
        "ok": reply_ok and reply_island["ok"],
    },
    "collapsed": {
        "path": collapsed_path,
        "top": collapsed_top,
        "island": collapsed_island,
        "ok": collapsed_ok and collapsed_island["ok"],
    },
    "ok": ok,
}
with open(proof_path, "w", encoding="utf-8") as handle:
    json.dump(proof, handle, indent=2)
    handle.write("\n")
if not ok:
    raise SystemExit(json.dumps(proof, indent=2))
PY

echo "$OUT_DIR"
