#!/usr/bin/env bash
set -euo pipefail

export CUA_DEV_HTTP_TOKEN_OVERRIDE=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v curl >/dev/null
command -v jq >/dev/null
command -v python3 >/dev/null

RUN_ID="$(date +%s)"
OUT_DIR="${CUA_VOICE_RECORDING_WATCH_LOOP_OUT_DIR:-artifacts/cua/voice-recording-watch-loop-$RUN_ID}"
PROFILE="${CUA_VOICE_RECORDING_WATCH_LOOP_PROFILE:-voice-recording-watch-loop-$RUN_ID}"
ADDR="${CUA_VOICE_RECORDING_WATCH_LOOP_ADDR:-127.0.0.1:$((23000 + RUN_ID % 1000))}"
MODEL_ADDR="${CUA_VOICE_RECORDING_WATCH_LOOP_MODEL_ADDR:-127.0.0.1:$((24000 + RUN_ID % 1000))}"
TOKEN="${CUA_HTTP_TOKEN:-voice-recording-watch-loop-token-$RUN_ID}"
CUA_HOME_DIR="${CUA_VOICE_RECORDING_WATCH_LOOP_HOME:-}"
VIDEO="$OUT_DIR/agent-screen.mp4"
VOICE_TRACE="$OUT_DIR/voice.jsonl"
EVENTS="$OUT_DIR/events.jsonl"
REQUESTS="$OUT_DIR/mock-openrouter.jsonl"
PROOF="$OUT_DIR/proof.json"

mkdir -p "$OUT_DIR"
if [[ -z "$CUA_HOME_DIR" ]]; then
  CUA_HOME_DIR="$(mktemp -d /tmp/cuavrw.XXXXXX)"
else
  mkdir -p "$CUA_HOME_DIR"
fi
mkdir -p "$CUA_HOME_DIR/config"
: > "$CUA_HOME_DIR/config/env"

SDKROOT="${SDKROOT:-$(xcrun --sdk macosx --show-sdk-path)}" \
BINDGEN_EXTRA_CLANG_ARGS="${BINDGEN_EXTRA_CLANG_ARGS:--isysroot $(xcrun --sdk macosx --show-sdk-path)}" \
cargo build -p cua -p cua-voice

if [[ -n "${CUA_BIN:-}" ]]; then
  CUA_BIN_PATH="$CUA_BIN"
else
  CUA_BIN_PATH="$(find target -path '*/debug/cua' -type f | head -n 1)"
fi
if [[ -n "${CUA_VOICE_BIN:-}" ]]; then
  VOICE_BIN_PATH="$CUA_VOICE_BIN"
else
  VOICE_BIN_PATH="$(find target -path '*/debug/cua-voice' -type f | head -n 1)"
fi
if [[ -z "$CUA_BIN_PATH" || ! -x "$CUA_BIN_PATH" ]]; then
  echo "cua binary not found" >&2
  exit 1
fi
if [[ -z "$VOICE_BIN_PATH" || ! -x "$VOICE_BIN_PATH" ]]; then
  echo "cua-voice binary not found" >&2
  exit 1
fi

python3 - "$MODEL_ADDR" "$REQUESTS" "$CUA_BIN_PATH" "$PROFILE" "$VIDEO" <<'PY' &
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

host, port_text = sys.argv[1].rsplit(":", 1)
requests_path, cua_bin, profile, video = sys.argv[2:6]
counter = 0

class Handler(BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        return

    def do_POST(self):
        global counter
        raw = self.rfile.read(int(self.headers.get("content-length", "0"))).decode("utf-8")
        body = json.loads(raw)
        request_text = json.dumps(body)
        counter += 1
        contains_recording = "cua.recording.v1" in request_text
        contains_inspection = "cua.video_inspection.v1" in request_text
        with open(requests_path, "a", encoding="utf-8") as handle:
            handle.write(json.dumps({
                "request": counter,
                "contains_recording_evidence": contains_recording,
                "contains_inspection_evidence": contains_inspection,
            }) + "\n")
        if counter == 1:
            content = json.dumps({
                "response": "Recording and inspecting a short screen clip.",
                "action": {
                    "kind": "shell_exec",
                    "command": f"{cua_bin} --profile {profile} record --out {video} --duration-ms 1000 --fps 5 --max-width 640 --inspect-frames 2 --json && {cua_bin} video inspect {video} --frames 2 --max-width 640 --json",
                    "timeout_ms": 60000,
                },
            })
        elif contains_recording and contains_inspection:
            content = json.dumps({
                "response": "The verified recording report includes schema_version cua.recording.v1 and the verified video inspection report includes schema_version cua.video_inspection.v1.",
                "action": None,
            })
        else:
            content = json.dumps({
                "response": "Evidence was incomplete; reading the manifest path would be required.",
                "action": None,
            })
        response = {
            "choices": [{"message": {"role": "assistant", "content": content}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        }
        payload = json.dumps(response).encode("utf-8")
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

ThreadingHTTPServer((host, int(port_text)), Handler).serve_forever()
PY
MODEL_PID="$!"

CUA_HOME="$CUA_HOME_DIR" \
CUA_HTTP_TOKEN="$TOKEN" \
"$CUA_BIN_PATH" --server-addr "$ADDR" --profile "$PROFILE" serve --addr "$ADDR" \
  >"$OUT_DIR/daemon.stdout" 2>"$OUT_DIR/daemon.stderr" &
DAEMON_PID="$!"

cleanup() {
  kill "$DAEMON_PID" "$MODEL_PID" >/dev/null 2>&1 || true
  wait "$DAEMON_PID" "$MODEL_PID" >/dev/null 2>&1 || true
}
trap cleanup EXIT

for _ in $(seq 1 80); do
  if curl -fs "http://$ADDR/healthz" >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done
curl -fsS "http://$ADDR/healthz" >"$OUT_DIR/status.json"

CUA_HOME="$CUA_HOME_DIR" \
CUA_HTTP_TOKEN="$TOKEN" \
CUA_VOICE_DEBUG_TRACE=true \
CUA_VOICE_TRACE_PATH="$VOICE_TRACE" \
CUA_VOICE_PLANNER_CHAT_COMPLETIONS_URL="http://$MODEL_ADDR/v1/chat/completions" \
OPENROUTER_API_KEY="mock-key" \
"$VOICE_BIN_PATH" \
  --headless \
  --profile "$PROFILE" \
  --planner-model anthropic/claude-sonnet-4.6 \
  --once-transcript "Screen record what is visible, watch the video, and answer what happened." \
  >"$EVENTS" 2>"$OUT_DIR/voice.stderr"

jq -s '
  {
    events: map(.event),
    reply: (map(select(.event == "reply") | .data.text) | last),
    stop: (map(select(.event == "agent_loop_stop") | .data) | last),
    saw_dispatch: any(.event == "dispatch_result" and ((.data.result.evidence | tostring) | contains("cua.recording.v1")) and ((.data.result.evidence | tostring) | contains("cua.video_inspection.v1"))),
    dispatch_message: (map(select(.event == "dispatch_result") | .data.result.evidence[0].message) | last)
  }
' "$VOICE_TRACE" >"$OUT_DIR/voice-summary.json"

jq -n \
  --arg schema_version "cua.voice_recording_watch_loop_proof.v1" \
  --arg profile "$PROFILE" \
  --arg out_dir "$OUT_DIR" \
  --slurpfile requests "$REQUESTS" \
  --slurpfile voice "$OUT_DIR/voice-summary.json" \
  '{
    schema_version: $schema_version,
    profile: $profile,
    out_dir: $out_dir,
    planner_requests: ($requests | length),
    second_request_has_recording_evidence: ($requests[1].contains_recording_evidence == true),
    second_request_has_inspection_evidence: ($requests[1].contains_inspection_evidence == true),
    trace_stop: $voice[0].stop,
    final_reply: $voice[0].reply,
    saw_dispatch: $voice[0].saw_dispatch,
    dispatch_message: $voice[0].dispatch_message,
    ok: (
      ($requests | length) == 2 and
      ($requests[1].contains_recording_evidence == true) and
      ($requests[1].contains_inspection_evidence == true) and
      ($voice[0].stop.attempts == 2) and
      ($voice[0].reply | contains("cua.video_inspection.v1")) and
      ($voice[0].saw_dispatch == true)
    )
  }' >"$PROOF"

jq -e '.ok == true' "$PROOF" >/dev/null
echo "$OUT_DIR"
