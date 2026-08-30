#!/usr/bin/env bash
set -euo pipefail

export CUA_DEV_HTTP_TOKEN_OVERRIDE=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v curl >/dev/null
command -v jq >/dev/null
command -v python3 >/dev/null

RUN_ID="$(date +%s)"
OUT_DIR="${CUA_VOICE_VISIBLE_NO_PROGRESS_OUT_DIR:-artifacts/cua/voice-visible-no-progress-$RUN_ID}"
PROFILE="${CUA_VOICE_VISIBLE_NO_PROGRESS_PROFILE:-voice-visible-no-progress-$RUN_ID}"
ADDR="${CUA_VOICE_VISIBLE_NO_PROGRESS_ADDR:-127.0.0.1:$((25000 + RUN_ID % 1000))}"
MODEL_ADDR="${CUA_VOICE_VISIBLE_NO_PROGRESS_MODEL_ADDR:-127.0.0.1:$((26000 + RUN_ID % 1000))}"
TOKEN="${CUA_HTTP_TOKEN:-voice-visible-no-progress-token-$RUN_ID}"
CUA_HOME_DIR="${CUA_VOICE_VISIBLE_NO_PROGRESS_HOME:-}"
VOICE_TRACE="$OUT_DIR/voice.jsonl"
EVENTS="$OUT_DIR/events.jsonl"
REQUESTS="$OUT_DIR/mock-openrouter.jsonl"
PROOF="$OUT_DIR/proof.json"

mkdir -p "$OUT_DIR"
if [[ -z "$CUA_HOME_DIR" ]]; then
  CUA_HOME_DIR="$(mktemp -d /tmp/cuavnp.XXXXXX)"
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

python3 - "$MODEL_ADDR" "$REQUESTS" <<'PY' &
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

host, port_text = sys.argv[1].rsplit(":", 1)
requests_path = sys.argv[2]
counter = 0

class Handler(BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        return

    def do_POST(self):
        global counter
        raw = self.rfile.read(int(self.headers.get("content-length", "0"))).decode("utf-8")
        body = json.loads(raw)
        counter += 1
        request_text = json.dumps(body)
        with open(requests_path, "a", encoding="utf-8") as handle:
            handle.write(json.dumps({
                "request": counter,
                "path": self.path,
                "contains_prior_attempts": "Prior attempts in this turn" in request_text,
                "contains_verification_observation": "verification_observation" in request_text,
                "contains_frame_changed": "frame_changed" in request_text,
                "contains_focused_window": "focused_window" in request_text,
                "contains_five_turn_limit": "five turn" in request_text.lower() or "5-turn" in request_text.lower(),
                "body": body,
            }) + "\n")
        content = json.dumps({
            "response": f"Attempt {counter}: moving the pointer to check whether the visible target responds.",
            "action": {
                "kind": "mouse_move",
                "x": 1,
                "y": 1,
                "duration_ms": 0,
            },
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

for _ in $(seq 1 80); do
  if python3 - "$MODEL_ADDR" <<'PY' >/dev/null 2>&1
import socket
import sys
host, port = sys.argv[1].rsplit(":", 1)
with socket.create_connection((host, int(port)), timeout=0.1):
    pass
PY
  then
    break
  fi
  sleep 0.1
done

set +e
CUA_HOME="$CUA_HOME_DIR" \
CUA_HTTP_TOKEN="$TOKEN" \
CUA_VOICE_DEBUG_TRACE=true \
CUA_VOICE_TRACE_PATH="$VOICE_TRACE" \
CUA_VOICE_PLANNER_CHAT_COMPLETIONS_URL="http://$MODEL_ADDR/v1/chat/completions" \
CUA_AGENT_LOOP_MAX_ATTEMPTS="n" \
OPENROUTER_API_KEY="mock-key" \
"$VOICE_BIN_PATH" \
  --headless \
  --profile "$PROFILE" \
  --once-transcript "Use the visible screen to complete a task, but stop if repeated visible actions do not produce verifiable progress." \
  >"$EVENTS" 2>"$OUT_DIR/voice.stderr"
VOICE_EXIT=$?
set -e

test "$VOICE_EXIT" -eq 0

jq -s '
  {
    events: map(.event),
    reply: (map(select(.event == "reply") | .data.text) | last),
    start: (map(select(.event == "turn_start") | .data) | last),
    stalled: (map(select(.event == "agent_loop_stalled") | .data) | last),
    stop: (map(select(.event == "agent_loop_stop") | .data) | last),
    outcomes: (map(select(.event == "agent_attempt_outcome") | .data)),
    terminal: (map(select(.event == "turn_complete") | .data) | last)
  }
' "$VOICE_TRACE" >"$OUT_DIR/voice-summary.json"

jq -n \
  --arg schema_version "cua.voice_visible_no_progress_proof.v1" \
  --arg profile "$PROFILE" \
  --arg out_dir "$OUT_DIR" \
  --slurpfile requests "$REQUESTS" \
  --slurpfile voice "$OUT_DIR/voice-summary.json" \
  --rawfile trace "$VOICE_TRACE" \
  '{
    schema_version: $schema_version,
    profile: $profile,
    out_dir: $out_dir,
    planner_requests: ($requests | length),
    default_planner_model: $voice[0].start.config.planner_model,
    final_reply: $voice[0].reply,
    stalled: $voice[0].stalled,
    stop: $voice[0].stop,
    outcome_count: ($voice[0].outcomes | length),
    has_terminal_turn_complete: ($voice[0].terminal != null),
    second_request_has_mini_turn_context: (
      ($requests[1].contains_prior_attempts == true) and
      ($requests[1].contains_verification_observation == true) and
      ($requests[1].contains_frame_changed == true) and
      ($requests[1].contains_focused_window == true)
    ),
    later_request_has_mini_turn_context: (
      ($requests[2].contains_prior_attempts == true) and
      ($requests[2].contains_verification_observation == true) and
      ($requests[2].contains_frame_changed == true) and
      ($requests[2].contains_focused_window == true)
    ),
    no_five_turn_limit_leaked: (all($requests[]; .contains_five_turn_limit == false) and (($trace | ascii_downcase | contains("five turn")) | not) and (($trace | ascii_downcase | contains("5-turn")) | not)),
    ok: (
      ($requests | length) == 3 and
      $voice[0].start.config.planner_model == "anthropic/claude-sonnet-4.6" and
      ($voice[0].outcomes | length) == 3 and
      ($voice[0].stalled.reason == "visible_action_without_observed_progress") and
      ($voice[0].stalled.consecutive_attempts == 3) and
      ($voice[0].stop.final_effect == "partial") and
      ($voice[0].reply | contains("repeated visible actions")) and
      ($voice[0].terminal != null) and
      ($requests[1].contains_verification_observation == true) and
      ($requests[2].contains_verification_observation == true) and
      (all($requests[]; .contains_five_turn_limit == false))
    )
  }' >"$PROOF"

jq -e '
  .ok == true and
  .planner_requests == 3 and
  .default_planner_model == "anthropic/claude-sonnet-4.6" and
  .outcome_count == 3 and
  .has_terminal_turn_complete == true and
  .second_request_has_mini_turn_context == true and
  .later_request_has_mini_turn_context == true and
  .no_five_turn_limit_leaked == true
' "$PROOF" >/dev/null

echo "$OUT_DIR"
