#!/usr/bin/env bash
set -euo pipefail

export CUA_DEV_HTTP_TOKEN_OVERRIDE=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

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

require_cmd() {
  command -v "$1" >/dev/null || {
    echo "$1 is required" >&2
    exit 1
  }
}

require_cmd ffmpeg
require_cmd ffprobe
require_cmd jq
require_cmd python3

RUN_ID="$(date +%s)"
PROFILE="${CUA_RECORDING_WATCH_PROOF_PROFILE:-recording-watch-proof-$RUN_ID}"
ADDR="${CUA_RECORDING_WATCH_PROOF_ADDR:-127.0.0.1:$((24000 + RUN_ID % 1000))}"
TOKEN="${CUA_HTTP_TOKEN:-recording-watch-proof-token-$RUN_ID}"
OUT_DIR="${CUA_RECORDING_WATCH_PROOF_OUT_DIR:-artifacts/cua/recording-watch-proof-$RUN_ID}"
VIDEO="$OUT_DIR/screen.mp4"
RECORD_JSON="$OUT_DIR/record.json"
INSPECT_JSON="$OUT_DIR/inspect.json"
FFPROBE_JSON="$OUT_DIR/ffprobe.json"
WATCH_TXT="$OUT_DIR/watch.txt"
WATCH_ERR="$OUT_DIR/watch.stderr"
PROOF_JSON="$OUT_DIR/proof.json"
WATCH_PY="${CUA_WATCH_PY:-/Users/deepsaint/.codex/skills/agent-watch/scripts/watch.py}"

cargo build -p cua >/dev/null

if [[ -n "${CUA_BIN:-}" ]]; then
  CUA_BIN_PATH="$CUA_BIN"
else
  CUA_BIN_PATH="$(debug_bin cua)"
fi

if [[ -z "$CUA_BIN_PATH" || ! -x "$CUA_BIN_PATH" ]]; then
  echo "cua binary not found" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"
CUA_HOME_DIR="${CUA_RECORDING_WATCH_PROOF_HOME:-$(mktemp -d /tmp/cuarw.XXXXXX)}"

cleanup() {
  if [[ -n "${DAEMON_PID:-}" ]]; then
    kill "$DAEMON_PID" 2>/dev/null || true
    wait "$DAEMON_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" \
  --server-addr "$ADDR" \
  --profile "$PROFILE" \
  serve --addr "$ADDR" --hud-mode headless \
  > "$OUT_DIR/daemon.stdout" 2> "$OUT_DIR/daemon.stderr" &
DAEMON_PID=$!

for _ in $(seq 1 500); do
  if CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" \
    --profile "$PROFILE" status --json > "$OUT_DIR/status.json" 2>/dev/null; then
    break
  fi
  sleep 0.02
done

jq -e '.schema_version == "cua.v1"' "$OUT_DIR/status.json" >/dev/null

CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" \
  --profile "$PROFILE" \
  record --out "$VIDEO" --duration-ms 1200 --fps 5 --max-width 640 --keep-frames --inspect-frames 3 --json \
  > "$RECORD_JSON"

CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" \
  video inspect "$VIDEO" --frames 3 --max-width 640 --json \
  > "$INSPECT_JSON"

ffprobe -v error -show_format -show_streams -of json "$VIDEO" > "$FFPROBE_JSON"

python3 "$WATCH_PY" "$VIDEO" --detail efficient --max-frames 5 --no-whisper \
  > "$WATCH_TXT" 2> "$WATCH_ERR"

jq -e '.ok == true and .frames_captured >= 1 and .inspection.ocr_requested == true and (.inspection.sampled_frames | length) >= 1' "$RECORD_JSON" >/dev/null
jq -e '.ok == true and .frames_extracted >= 1 and (.inspection.sampled_frames | length) >= 1' "$INSPECT_JSON" >/dev/null
jq -e 'any(.streams[]?; .codec_type == "video" and .codec_name == "h264" and .width > 0 and .height > 0)' "$FFPROBE_JSON" >/dev/null
grep -q "watch: video report" "$WATCH_TXT"
grep -q "Frames live at:" "$WATCH_TXT"
test -s "$VIDEO"
test -s "$VIDEO.json"

jq -n \
  --arg profile "$PROFILE" \
  --arg out_dir "$OUT_DIR" \
  --slurpfile record "$RECORD_JSON" \
  --slurpfile inspect "$INSPECT_JSON" \
  --slurpfile ffprobe "$FFPROBE_JSON" \
  '{
    schema_version: "cua.recording_watch_proof.v1",
    ok: true,
    profile: $profile,
    out_dir: $out_dir,
    recording: {
      video_path: $record[0].video_path,
      manifest_path: $record[0].manifest_path,
      frames_captured: $record[0].frames_captured,
      ocr_available: $record[0].inspection.ocr_available
    },
    inspection: {
      frames_extracted: $inspect[0].frames_extracted,
      sampled_frames: ($inspect[0].inspection.sampled_frames | length)
    },
    ffprobe_video_streams: [$ffprobe[0].streams[]? | select(.codec_type == "video") | {codec_name,width,height,nb_frames}]
  }' > "$PROOF_JSON"

printf '%s\n' "$OUT_DIR"
