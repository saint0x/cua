#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v curl >/dev/null
command -v jq >/dev/null

RUN_ID="$(date +%s)"
PROFILE="${CUA_CONTROL_SURFACE_PROOF_PROFILE:-host-control-surface-proof-$RUN_ID}"
ADDR="${CUA_CONTROL_SURFACE_PROOF_ADDR:-127.0.0.1:$((20000 + RUN_ID % 1000))}"
TOKEN="${CUA_HTTP_TOKEN:-host-control-surface-proof-token-$RUN_ID}"
OUT_DIR="${CUA_CONTROL_SURFACE_PROOF_OUT_DIR:-artifacts/cua/control-surface-proof-$RUN_ID}"
TRACE_DIR="$OUT_DIR/trace"
HTTP_STATUS="$OUT_DIR/http-status.json"
HTTP_MANIFEST="$OUT_DIR/http-manifest.json"
HTTP_METRICS="$OUT_DIR/http-metrics.json"
HTTP_EVENTS="$OUT_DIR/http-events.json"
HTTP_EVENTS_AFTER="$OUT_DIR/http-events-after.json"
HTTP_UI_STEP="$OUT_DIR/http-ui-step.json"
HTTP_UI_REPLY="$OUT_DIR/http-ui-reply.json"
HTTP_OBSERVE="$OUT_DIR/http-observe.json"
HTTP_CONTEXT="$OUT_DIR/http-context.json"
HTTP_SCREENSHOT="$OUT_DIR/http-screenshot.json"
HTTP_FRAME_ACTION="$OUT_DIR/http-frame-action.json"
HTTP_CURSOR_AFTER_FRAME_ACTION="$OUT_DIR/http-cursor-after-frame-action.json"
CLI_STATUS="$OUT_DIR/cli-status.json"
CLI_MANIFEST="$OUT_DIR/cli-manifest.json"
CLI_METRICS="$OUT_DIR/cli-metrics.json"
CLI_EVENTS="$OUT_DIR/cli-events.json"
CLI_EVENTS_AFTER="$OUT_DIR/cli-events-after.json"
CLI_UI_STEP="$OUT_DIR/cli-ui-step.json"
CLI_UI_REPLY="$OUT_DIR/cli-ui-reply.json"
CLI_OBSERVE="$OUT_DIR/cli-observe.json"
CLI_CONTEXT="$OUT_DIR/cli-context.json"
CLI_PROFILE="$OUT_DIR/cli-profile.json"
CLI_PAUSE="$OUT_DIR/cli-pause.json"
CLI_RESUME="$OUT_DIR/cli-resume.json"
CLI_SCREENSHOT_JSON="$OUT_DIR/cli-screenshot.json"
CLI_SCREENSHOT_PNG="$OUT_DIR/cli-screenshot.png"
CLI_STREAM="$OUT_DIR/cli-stream.jsonl"
UNIX_STATUS="$OUT_DIR/unix-status.json"
UNIX_MANIFEST="$OUT_DIR/unix-manifest.json"
UNIX_METRICS="$OUT_DIR/unix-metrics.json"
UNIX_EVENTS="$OUT_DIR/unix-events.json"
UNIX_EVENTS_AFTER="$OUT_DIR/unix-events-after.json"
UNIX_EVENTS_WAIT="$OUT_DIR/unix-events-wait.json"
UNIX_UI_STEP="$OUT_DIR/unix-ui-step.json"
UNIX_UI_REPLY="$OUT_DIR/unix-ui-reply.json"
VOICE_AGENT_STEP="$OUT_DIR/voice-agent-step.json"
VOICE_AGENT_REPLY="$OUT_DIR/voice-agent-reply.json"
UNIX_CONTEXT="$OUT_DIR/unix-context.json"
UNIX_PAUSE="$OUT_DIR/unix-pause.json"
UNIX_RESUME="$OUT_DIR/unix-resume.json"
PROOF="$OUT_DIR/proof.json"

cargo build -p cua -p cua-voice

if [[ -n "${CUA_BIN:-}" ]]; then
  CUA_BIN_PATH="$CUA_BIN"
elif [[ -x target/debug/cua ]]; then
  CUA_BIN_PATH="target/debug/cua"
else
  CUA_BIN_PATH="$(find target -path '*/debug/cua' -type f 2>/dev/null | head -n 1)"
fi

if [[ -z "$CUA_BIN_PATH" || ! -x "$CUA_BIN_PATH" ]]; then
  echo "cua binary not found" >&2
  exit 1
fi

if [[ -n "${CUA_VOICE_BIN:-}" ]]; then
  CUA_VOICE_BIN_PATH="$CUA_VOICE_BIN"
elif [[ -x target/debug/cua-voice ]]; then
  CUA_VOICE_BIN_PATH="target/debug/cua-voice"
else
  CUA_VOICE_BIN_PATH="$(find target -path '*/debug/cua-voice' -type f 2>/dev/null | head -n 1)"
fi

if [[ -z "$CUA_VOICE_BIN_PATH" || ! -x "$CUA_VOICE_BIN_PATH" ]]; then
  echo "cua-voice binary not found" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"
SOCKET_PATH="$HOME/.cua/profiles/$PROFILE/daemon.sock"
CUA_HTTP_TOKEN="$TOKEN" CUA_TRACE_DIR="$TRACE_DIR" "$CUA_BIN_PATH" \
  --server-addr "$ADDR" \
  --profile "$PROFILE" \
  serve --addr "$ADDR" &
DAEMON_PID="$!"

cleanup() {
  kill "$DAEMON_PID" >/dev/null 2>&1 || true
  wait "$DAEMON_PID" >/dev/null 2>&1 || true
}
trap cleanup EXIT

for _ in $(seq 1 80); do
  if curl -fs "http://$ADDR/healthz" >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done
curl -fsS "http://$ADDR/healthz" >/dev/null

unix_call() {
  local method="$1"
  local params="$2"
  local out="$3"
  python3 - "$SOCKET_PATH" "$TOKEN" "$method" "$params" > "$out" <<'PY'
import json
import socket
import sys
import uuid

path, token, method, params_json = sys.argv[1:5]
request = {
    "id": str(uuid.uuid4()),
    "token": token,
    "method": method,
    "params": json.loads(params_json),
}
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
    raise SystemExit(json.dumps(response))
print(json.dumps(response["result"]))
PY
}

http_json_call() {
  local path="$1"
  local body="$2"
  local out="$3"
  local attempts="${4:-1}"
  local delay="${5:-0.25}"
  local tmp="$out.tmp"

  for attempt in $(seq 1 "$attempts"); do
    if curl -fsS \
      -H "authorization: Bearer $TOKEN" \
      -H "content-type: application/json" \
      -d "$body" \
      "http://$ADDR$path" > "$tmp"; then
      mv "$tmp" "$out"
      return 0
    fi
    rm -f "$tmp"
    if [[ "$attempt" != "$attempts" ]]; then
      sleep "$delay"
    fi
  done

  return 1
}

curl -fsS -H "authorization: Bearer $TOKEN" "http://$ADDR/status" > "$HTTP_STATUS"
curl -fsS -H "authorization: Bearer $TOKEN" "http://$ADDR/manifest" > "$HTTP_MANIFEST"
curl -fsS -H "authorization: Bearer $TOKEN" "http://$ADDR/metrics" > "$HTTP_METRICS"
curl -fsS \
  -H "authorization: Bearer $TOKEN" \
  -H "content-type: application/json" \
  -d '{"schema_version":"cua.v1","label":"http programmable step","source":"http proof","task":"http task","tool":"http tool","step_index":2,"step_total":5,"ttl_ms":1500}' \
  "http://$ADDR/ui/step" > "$HTTP_UI_STEP"
curl -fsS \
  -H "authorization: Bearer $TOKEN" \
  -H "content-type: application/json" \
  -d '{"schema_version":"cua.v1","text":"http programmable reply","source":"http proof","ttl_ms":1750}' \
  "http://$ADDR/ui/reply" > "$HTTP_UI_REPLY"
curl -fsS -H "authorization: Bearer $TOKEN" "http://$ADDR/events" > "$HTTP_EVENTS"
HTTP_AFTER_SEQUENCE="$(jq -r '.[0].sequence' "$HTTP_EVENTS")"
curl -fsS -H "authorization: Bearer $TOKEN" "http://$ADDR/events?after=$HTTP_AFTER_SEQUENCE" > "$HTTP_EVENTS_AFTER"
curl -fsS -H "authorization: Bearer $TOKEN" "http://$ADDR/observe/desktop" > "$HTTP_OBSERVE"
http_json_call "/context/snapshot" \
  '{"max_width":640,"encoding":"png","force_fresh":true,"include_bytes":false}' \
  "$HTTP_CONTEXT" \
  5 \
  0.5
http_json_call "/capture/screenshot" \
  '{"max_width":640,"encoding":"png","force_fresh":true,"include_bytes":false}' \
  "$HTTP_SCREENSHOT" \
  5 \
  0.5
curl -fsS \
  -H "authorization: Bearer $TOKEN" \
  -H "content-type: application/json" \
  -d "$(jq -c '{schema_version:"cua.v1",source_frame:.envelope,action:{kind:"mouse_move",x:100,y:100,duration_ms:0}}' "$HTTP_SCREENSHOT")" \
  "http://$ADDR/input/frame" > "$HTTP_FRAME_ACTION"
sleep 0.2
curl -fsS -H "authorization: Bearer $TOKEN" "http://$ADDR/observe/cursor" > "$HTTP_CURSOR_AFTER_FRAME_ACTION"

CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --server-addr "$ADDR" --profile "$PROFILE" status --json > "$CLI_STATUS"
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --server-addr "$ADDR" --profile "$PROFILE" manifest --json > "$CLI_MANIFEST"
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --server-addr "$ADDR" --profile "$PROFILE" metrics --json > "$CLI_METRICS"
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --server-addr "$ADDR" --profile "$PROFILE" ui step "cli programmable step" \
  --source "cli proof" \
  --task "cli task" \
  --tool "cli tool" \
  --step-index 3 \
  --step-total 5 \
  --ttl-ms 1500 \
  --json > "$CLI_UI_STEP"
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --server-addr "$ADDR" --profile "$PROFILE" ui reply "cli programmable reply" \
  --source "cli proof" \
  --ttl-ms 1750 \
  --json > "$CLI_UI_REPLY"
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --server-addr "$ADDR" --profile "$PROFILE" events --json > "$CLI_EVENTS"
CLI_AFTER_SEQUENCE="$(jq -r '.[0].sequence' "$CLI_EVENTS")"
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --server-addr "$ADDR" --profile "$PROFILE" events --after "$CLI_AFTER_SEQUENCE" --json > "$CLI_EVENTS_AFTER"
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --server-addr "$ADDR" --profile "$PROFILE" observe --json > "$CLI_OBSERVE"
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --server-addr "$ADDR" --profile "$PROFILE" context --json --max-width 640 --force-fresh > "$CLI_CONTEXT"
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --server-addr "$ADDR" --profile "$PROFILE" profile status --json > "$CLI_PROFILE"
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --server-addr "$ADDR" --profile "$PROFILE" pause --json > "$CLI_PAUSE"
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --server-addr "$ADDR" --profile "$PROFILE" resume --json > "$CLI_RESUME"
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --server-addr "$ADDR" --profile "$PROFILE" screenshot \
  --out "$CLI_SCREENSHOT_PNG" \
  --max-width 640 \
  --force-fresh \
  --json > "$CLI_SCREENSHOT_JSON"
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --server-addr "$ADDR" --profile "$PROFILE" stream \
  --unix \
  --frames 2 \
  --fps 5 \
  --json > "$CLI_STREAM"

unix_call "status" '{}' "$UNIX_STATUS"
unix_call "manifest" '{}' "$UNIX_MANIFEST"
unix_call "metrics" '{}' "$UNIX_METRICS"
unix_call "ui.step" '{"schema_version":"cua.v1","label":"unix programmable step","source":"unix proof","task":"unix task","tool":"unix tool","step_index":4,"step_total":5,"ttl_ms":1500}' "$UNIX_UI_STEP"
unix_call "ui.reply" '{"schema_version":"cua.v1","text":"unix programmable reply","source":"unix proof","ttl_ms":1750}' "$UNIX_UI_REPLY"
unix_call "events.snapshot" '{}' "$UNIX_EVENTS"
UNIX_AFTER_SEQUENCE="$(jq -r '.[0].sequence' "$UNIX_EVENTS")"
unix_call "events.after" "{\"after_sequence\":$UNIX_AFTER_SEQUENCE}" "$UNIX_EVENTS_AFTER"
unix_call "events.wait" "{\"after_sequence\":$UNIX_AFTER_SEQUENCE,\"timeout_ms\":25}" "$UNIX_EVENTS_WAIT"
VOICE_AFTER_SEQUENCE="$(jq -r 'map(.sequence) | max // 0' "$UNIX_EVENTS")"
unix_call "ui.step" '{"schema_version":"cua.v1","label":"voice bridge programmable step","source":"external agent","task":"voice bridge task","tool":"agent tool","step_index":5,"step_total":7,"ttl_ms":1750}' "$OUT_DIR/voice-bridge-ui-step.json" >/dev/null
CUA_HTTP_TOKEN="$TOKEN" "$CUA_VOICE_BIN_PATH" \
  --profile "$PROFILE" \
  --once-agent-step-after "$VOICE_AFTER_SEQUENCE" \
  --once-agent-step-wait-ms 2000 > "$VOICE_AGENT_STEP"
VOICE_REPLY_AFTER_SEQUENCE="$(jq -r 'map(.sequence) | max // 0' "$UNIX_EVENTS")"
unix_call "ui.reply" '{"schema_version":"cua.v1","text":"voice bridge programmable reply","source":"external agent","ttl_ms":1750}' "$OUT_DIR/voice-bridge-ui-reply.json" >/dev/null
CUA_HTTP_TOKEN="$TOKEN" "$CUA_VOICE_BIN_PATH" \
  --profile "$PROFILE" \
  --once-agent-reply-after "$VOICE_REPLY_AFTER_SEQUENCE" \
  --once-agent-reply-wait-ms 2000 > "$VOICE_AGENT_REPLY"
unix_call "context.snapshot" '{"max_width":640,"encoding":"png","force_fresh":true,"include_bytes":false}' "$UNIX_CONTEXT"
unix_call "control.pause" '{}' "$UNIX_PAUSE"
unix_call "control.resume" '{}' "$UNIX_RESUME"

jq -e '.schema_version == "cua.v1" and .active_profile == $profile' \
  --arg profile "$PROFILE" "$HTTP_STATUS" >/dev/null
jq -e '(.public_surfaces | index("cli")) and (.public_surfaces | index("local_http")) and (.endpoints | index("POST /context/snapshot")) and (.endpoints | index("POST /input/frame")) and (.endpoints | index("UNIX visual.session"))' \
  "$HTTP_MANIFEST" >/dev/null
jq -e '.schema_version == "cua.v1" and (.histograms | type) == "array" and (.counters | type) == "object"' \
  "$HTTP_METRICS" >/dev/null
jq -e '.accepted == true and .label == "http programmable step" and .source == "http proof" and .task == "http task" and .tool == "http tool" and .step_index == 2 and .step_total == 5 and .ttl_ms == 1500' \
  "$HTTP_UI_STEP" >/dev/null
jq -e '.accepted == true and .text == "http programmable reply" and .source == "http proof" and .ttl_ms == 1750' \
  "$HTTP_UI_REPLY" >/dev/null
jq -e 'type == "array" and length >= 1 and .[0].kind == "daemon_started" and any(.[]; .kind == "ui_step" and .data.label == "http programmable step" and .data.task == "http task" and .data.tool == "http tool" and .data.step_index == 2 and .data.step_total == 5 and .data.ttl_ms == 1500) and any(.[]; .kind == "ui_reply" and .data.text == "http programmable reply" and .data.source == "http proof" and .data.ttl_ms == 1750)' \
  "$HTTP_EVENTS" >/dev/null
jq -e 'type == "array" and length >= 1 and all(.[]; .sequence > '"$HTTP_AFTER_SEQUENCE"') and any(.[]; .kind == "ui_step" and .data.label == "http programmable step" and .data.task == "http task" and .data.tool == "http tool" and .data.step_index == 2 and .data.step_total == 5 and .data.ttl_ms == 1500) and any(.[]; .kind == "ui_reply" and .data.text == "http programmable reply" and .data.source == "http proof" and .data.ttl_ms == 1750)' \
  "$HTTP_EVENTS_AFTER" >/dev/null
jq -e '(.displays | length) >= 1 and (.windows | type) == "array" and (.cursor.visible | type) == "boolean"' \
  "$HTTP_OBSERVE" >/dev/null
jq -e '.frame.envelope.encoding == "png" and .frame.envelope.width > 0 and (.frame.envelope.display_x | type) == "number" and (.frame.envelope.display_y | type) == "number" and (.frame.envelope.frame_origin_x | type) == "number" and (.frame.envelope.frame_origin_y | type) == "number" and (.desktop.displays | length) >= 1 and (.desktop.windows | type) == "array"' \
  "$HTTP_CONTEXT" >/dev/null
jq -e '.envelope.encoding == "png" and .envelope.width > 0 and .envelope.height > 0 and (.envelope.display_x | type) == "number" and (.envelope.display_y | type) == "number" and (.envelope.frame_origin_x | type) == "number" and (.envelope.frame_origin_y | type) == "number" and (.envelope.sha256 | length) > 0' \
  "$HTTP_SCREENSHOT" >/dev/null
EXPECTED_FRAME_X="$(jq '(.envelope.display_x + ((100 - .envelope.frame_origin_x) * (.envelope.display_width / .envelope.width)) | round)' "$HTTP_SCREENSHOT")"
EXPECTED_FRAME_Y="$(jq '(.envelope.display_y + ((100 - .envelope.frame_origin_y) * (.envelope.display_height / .envelope.height)) | round)' "$HTTP_SCREENSHOT")"
jq -e '.effect == "confirmed" and .route == "accessibility"' "$HTTP_FRAME_ACTION" >/dev/null
jq -e '.x == '"$EXPECTED_FRAME_X"' and .y == '"$EXPECTED_FRAME_Y" "$HTTP_CURSOR_AFTER_FRAME_ACTION" >/dev/null
jq -e '.schema_version == "cua.v1" and .active_profile == $profile' \
  --arg profile "$PROFILE" "$CLI_STATUS" >/dev/null
jq -e '(.public_surfaces | index("cli")) and (.public_surfaces | index("local_http")) and (.commands | index("cua context --json")) and (.commands | index("cua stream --unix --json"))' \
  "$CLI_MANIFEST" >/dev/null
jq -e '.schema_version == "cua.v1" and (.histograms | type) == "array" and (.counters | type) == "object"' \
  "$CLI_METRICS" >/dev/null
jq -e '.accepted == true and .label == "cli programmable step" and .source == "cli proof" and .task == "cli task" and .tool == "cli tool" and .step_index == 3 and .step_total == 5 and .ttl_ms == 1500' \
  "$CLI_UI_STEP" >/dev/null
jq -e '.accepted == true and .text == "cli programmable reply" and .source == "cli proof" and .ttl_ms == 1750' \
  "$CLI_UI_REPLY" >/dev/null
jq -e 'type == "array" and length >= 1 and .[0].kind == "daemon_started" and any(.[]; .kind == "ui_step" and .data.label == "cli programmable step" and .data.task == "cli task" and .data.tool == "cli tool" and .data.step_index == 3 and .data.step_total == 5 and .data.ttl_ms == 1500) and any(.[]; .kind == "ui_reply" and .data.text == "cli programmable reply" and .data.source == "cli proof" and .data.ttl_ms == 1750)' \
  "$CLI_EVENTS" >/dev/null
jq -e 'type == "array" and length >= 1 and all(.[]; .sequence > '"$CLI_AFTER_SEQUENCE"') and any(.[]; .kind == "ui_step" and .data.label == "cli programmable step" and .data.task == "cli task" and .data.tool == "cli tool" and .data.step_index == 3 and .data.step_total == 5 and .data.ttl_ms == 1500) and any(.[]; .kind == "ui_reply" and .data.text == "cli programmable reply" and .data.source == "cli proof" and .data.ttl_ms == 1750)' \
  "$CLI_EVENTS_AFTER" >/dev/null
jq -e '(.displays | length) >= 1 and (.windows | type) == "array" and (.cursor.visible | type) == "boolean"' \
  "$CLI_OBSERVE" >/dev/null
jq -e '.frame.envelope.encoding == "png" and .frame.envelope.width > 0 and (.desktop.displays | length) >= 1 and (.desktop.windows | type) == "array"' \
  "$CLI_CONTEXT" >/dev/null
jq -e '.active_profile.name == $profile' --arg profile "$PROFILE" "$CLI_PROFILE" >/dev/null
jq -e '.safety_state == "paused"' "$CLI_PAUSE" >/dev/null
jq -e '.safety_state == "running"' "$CLI_RESUME" >/dev/null
jq -e '.encoding == "png" and .width > 0 and .height > 0 and (.sha256 | length) > 0' \
  "$CLI_SCREENSHOT_JSON" >/dev/null
test -s "$CLI_SCREENSHOT_PNG"
jq -s -e 'map(select(.type == "frame")) | length == 2 and all(.[]; .frame.envelope.width > 0 and .frame.envelope.display_width > 0 and (.frame.envelope.display_x | type) == "number" and (.frame.envelope.frame_origin_x | type) == "number" and .frame.bytes_base64 == null)' \
  "$CLI_STREAM" >/dev/null
jq -e '.schema_version == "cua.v1" and .active_profile == $profile' \
  --arg profile "$PROFILE" "$UNIX_STATUS" >/dev/null
jq -e '(.public_surfaces | index("local_unix_socket")) and (.endpoints | index("POST /context/snapshot")) and (.endpoints | index("UNIX visual.session"))' \
  "$UNIX_MANIFEST" >/dev/null
jq -e '.schema_version == "cua.v1" and (.histograms | type) == "array" and (.counters | type) == "object"' \
  "$UNIX_METRICS" >/dev/null
jq -e '.accepted == true and .label == "unix programmable step" and .source == "unix proof" and .task == "unix task" and .tool == "unix tool" and .step_index == 4 and .step_total == 5 and .ttl_ms == 1500' \
  "$UNIX_UI_STEP" >/dev/null
jq -e '.accepted == true and .text == "unix programmable reply" and .source == "unix proof" and .ttl_ms == 1750' \
  "$UNIX_UI_REPLY" >/dev/null
jq -e 'type == "array" and length >= 1 and .[0].kind == "daemon_started" and any(.[]; .kind == "visual_session_started" and .data.fps == 5) and any(.[]; .kind == "ui_step" and .data.label == "unix programmable step" and .data.task == "unix task" and .data.tool == "unix tool" and .data.step_index == 4 and .data.step_total == 5 and .data.ttl_ms == 1500) and any(.[]; .kind == "ui_reply" and .data.text == "unix programmable reply" and .data.source == "unix proof" and .data.ttl_ms == 1750)' \
  "$UNIX_EVENTS" >/dev/null
jq -e 'type == "array" and length >= 1 and all(.[]; .sequence > '"$UNIX_AFTER_SEQUENCE"') and any(.[]; .kind == "ui_step" and .data.label == "unix programmable step" and .data.task == "unix task" and .data.tool == "unix tool" and .data.step_index == 4 and .data.step_total == 5 and .data.ttl_ms == 1500) and any(.[]; .kind == "ui_reply" and .data.text == "unix programmable reply" and .data.source == "unix proof" and .data.ttl_ms == 1750)' \
  "$UNIX_EVENTS_AFTER" >/dev/null
jq -e 'type == "array" and length >= 1 and all(.[]; .sequence > '"$UNIX_AFTER_SEQUENCE"') and any(.[]; .kind == "ui_step" and .data.label == "unix programmable step" and .data.step_index == 4 and .data.step_total == 5 and .data.ttl_ms == 1500) and any(.[]; .kind == "ui_reply" and .data.text == "unix programmable reply" and .data.source == "unix proof" and .data.ttl_ms == 1750)' \
  "$UNIX_EVENTS_WAIT" >/dev/null
jq -e '.event == "agent_step" and .label == "voice bridge programmable step" and .source == "external agent" and .task == "voice bridge task" and .tool == "agent tool" and .step_index == 5 and .step_total == 7 and .ttl_ms == 1750' \
  "$VOICE_AGENT_STEP" >/dev/null
jq -e '.event == "reply" and .text == "voice bridge programmable reply"' \
  "$VOICE_AGENT_REPLY" >/dev/null
jq -e '.frame.envelope.encoding == "png" and .frame.envelope.width > 0 and (.frame.envelope.display_x | type) == "number" and (.frame.envelope.display_y | type) == "number" and (.frame.envelope.frame_origin_x | type) == "number" and (.frame.envelope.frame_origin_y | type) == "number" and (.desktop.displays | length) >= 1 and (.desktop.windows | type) == "array"' \
  "$UNIX_CONTEXT" >/dev/null
jq -e '.safety_state == "paused"' "$UNIX_PAUSE" >/dev/null
jq -e '.safety_state == "running"' "$UNIX_RESUME" >/dev/null

jq -n \
  --arg profile "$PROFILE" \
  --arg addr "$ADDR" \
  --arg cli_screenshot "$CLI_SCREENSHOT_PNG" \
  --slurpfile http_status "$HTTP_STATUS" \
  --slurpfile http_manifest "$HTTP_MANIFEST" \
  --slurpfile http_metrics "$HTTP_METRICS" \
  --slurpfile http_events "$HTTP_EVENTS" \
  --slurpfile http_events_after "$HTTP_EVENTS_AFTER" \
  --slurpfile http_ui_step "$HTTP_UI_STEP" \
  --slurpfile http_ui_reply "$HTTP_UI_REPLY" \
  --slurpfile http_observe "$HTTP_OBSERVE" \
  --slurpfile http_context "$HTTP_CONTEXT" \
  --slurpfile http_screenshot "$HTTP_SCREENSHOT" \
  --slurpfile http_frame_action "$HTTP_FRAME_ACTION" \
  --slurpfile http_cursor_after_frame_action "$HTTP_CURSOR_AFTER_FRAME_ACTION" \
  --slurpfile cli_status "$CLI_STATUS" \
  --slurpfile cli_manifest "$CLI_MANIFEST" \
  --slurpfile cli_metrics "$CLI_METRICS" \
  --slurpfile cli_events "$CLI_EVENTS" \
  --slurpfile cli_events_after "$CLI_EVENTS_AFTER" \
  --slurpfile cli_ui_step "$CLI_UI_STEP" \
  --slurpfile cli_ui_reply "$CLI_UI_REPLY" \
  --slurpfile cli_observe "$CLI_OBSERVE" \
  --slurpfile cli_context "$CLI_CONTEXT" \
  --slurpfile cli_profile "$CLI_PROFILE" \
  --slurpfile cli_pause "$CLI_PAUSE" \
  --slurpfile cli_resume "$CLI_RESUME" \
  --slurpfile cli_screenshot_json "$CLI_SCREENSHOT_JSON" \
  --slurpfile cli_stream "$CLI_STREAM" \
  --slurpfile unix_status "$UNIX_STATUS" \
  --slurpfile unix_manifest "$UNIX_MANIFEST" \
  --slurpfile unix_metrics "$UNIX_METRICS" \
  --slurpfile unix_events "$UNIX_EVENTS" \
  --slurpfile unix_events_after "$UNIX_EVENTS_AFTER" \
  --slurpfile unix_events_wait "$UNIX_EVENTS_WAIT" \
  --slurpfile unix_ui_step "$UNIX_UI_STEP" \
  --slurpfile unix_ui_reply "$UNIX_UI_REPLY" \
  --slurpfile voice_agent_step "$VOICE_AGENT_STEP" \
  --slurpfile voice_agent_reply "$VOICE_AGENT_REPLY" \
  --slurpfile unix_context "$UNIX_CONTEXT" \
  --slurpfile unix_pause "$UNIX_PAUSE" \
  --slurpfile unix_resume "$UNIX_RESUME" \
  '{
    schema_version: "cua.control_surface_proof.v1",
    ok: true,
    profile: $profile,
    addr: $addr,
    http: {
      active_profile: $http_status[0].active_profile,
      endpoint_count: ($http_manifest[0].endpoints | length),
      histogram_count: ($http_metrics[0].histograms | length),
      event_count: ($http_events[0] | length),
      filtered_event_count: ($http_events_after[0] | length),
      ui_step: $http_ui_step[0].label,
      ui_task: $http_ui_step[0].task,
      ui_tool: $http_ui_step[0].tool,
      step_index: $http_ui_step[0].step_index,
      step_total: $http_ui_step[0].step_total,
      ttl_ms: $http_ui_step[0].ttl_ms,
      ui_reply: $http_ui_reply[0].text,
      ui_reply_source: $http_ui_reply[0].source,
      ui_reply_ttl_ms: $http_ui_reply[0].ttl_ms,
      display_count: ($http_observe[0].displays | length),
      window_count: ($http_observe[0].windows | length),
      context: {
        width: $http_context[0].frame.envelope.width,
        height: $http_context[0].frame.envelope.height,
        display_x: $http_context[0].frame.envelope.display_x,
        display_y: $http_context[0].frame.envelope.display_y,
        frame_origin_x: $http_context[0].frame.envelope.frame_origin_x,
        frame_origin_y: $http_context[0].frame.envelope.frame_origin_y,
        window_count: ($http_context[0].desktop.windows | length)
      },
      screenshot: {
        width: $http_screenshot[0].envelope.width,
        height: $http_screenshot[0].envelope.height,
        display_x: $http_screenshot[0].envelope.display_x,
        display_y: $http_screenshot[0].envelope.display_y,
        display_width: $http_screenshot[0].envelope.display_width,
        display_height: $http_screenshot[0].envelope.display_height,
        frame_origin_x: $http_screenshot[0].envelope.frame_origin_x,
        frame_origin_y: $http_screenshot[0].envelope.frame_origin_y,
        sha256: $http_screenshot[0].envelope.sha256
      },
      frame_action: {
        effect: $http_frame_action[0].effect,
        route: $http_frame_action[0].route,
        cursor_x: $http_cursor_after_frame_action[0].x,
        cursor_y: $http_cursor_after_frame_action[0].y
      }
    },
    cli: {
      active_profile: $cli_status[0].active_profile,
      command_count: ($cli_manifest[0].commands | length),
      histogram_count: ($cli_metrics[0].histograms | length),
      event_count: ($cli_events[0] | length),
      filtered_event_count: ($cli_events_after[0] | length),
      ui_step: $cli_ui_step[0].label,
      ui_task: $cli_ui_step[0].task,
      ui_tool: $cli_ui_step[0].tool,
      step_index: $cli_ui_step[0].step_index,
      step_total: $cli_ui_step[0].step_total,
      ttl_ms: $cli_ui_step[0].ttl_ms,
      ui_reply: $cli_ui_reply[0].text,
      ui_reply_source: $cli_ui_reply[0].source,
      ui_reply_ttl_ms: $cli_ui_reply[0].ttl_ms,
      profile_status: $cli_profile[0].active_profile.name,
      display_count: ($cli_observe[0].displays | length),
      window_count: ($cli_observe[0].windows | length),
      context: {
        width: $cli_context[0].frame.envelope.width,
        height: $cli_context[0].frame.envelope.height,
        display_x: $cli_context[0].frame.envelope.display_x,
        display_y: $cli_context[0].frame.envelope.display_y,
        frame_origin_x: $cli_context[0].frame.envelope.frame_origin_x,
        frame_origin_y: $cli_context[0].frame.envelope.frame_origin_y,
        window_count: ($cli_context[0].desktop.windows | length)
      },
      pause_state: $cli_pause[0].safety_state,
      resume_state: $cli_resume[0].safety_state,
      screenshot_path: $cli_screenshot,
      screenshot: {
        width: $cli_screenshot_json[0].width,
        height: $cli_screenshot_json[0].height,
        sha256: $cli_screenshot_json[0].sha256
      },
      stream: {
        frames: ($cli_stream | map(select(.type == "frame")) | length),
        width: ($cli_stream | map(select(.type == "frame")) | .[0].frame.envelope.width),
        display_x: ($cli_stream | map(select(.type == "frame")) | .[0].frame.envelope.display_x),
        display_width: ($cli_stream | map(select(.type == "frame")) | .[0].frame.envelope.display_width),
        frame_origin_x: ($cli_stream | map(select(.type == "frame")) | .[0].frame.envelope.frame_origin_x)
      }
    },
    unix: {
      active_profile: $unix_status[0].active_profile,
      endpoint_count: ($unix_manifest[0].endpoints | length),
      histogram_count: ($unix_metrics[0].histograms | length),
      event_count: ($unix_events[0] | length),
      visual_session_started_count: ($unix_events[0] | map(select(.kind == "visual_session_started")) | length),
      filtered_event_count: ($unix_events_after[0] | length),
      waited_event_count: ($unix_events_wait[0] | length),
      ui_step: $unix_ui_step[0].label,
      ui_task: $unix_ui_step[0].task,
      ui_tool: $unix_ui_step[0].tool,
      step_index: $unix_ui_step[0].step_index,
      step_total: $unix_ui_step[0].step_total,
      ttl_ms: $unix_ui_step[0].ttl_ms,
      ui_reply: $unix_ui_reply[0].text,
      ui_reply_source: $unix_ui_reply[0].source,
      ui_reply_ttl_ms: $unix_ui_reply[0].ttl_ms,
      voice_bridge: {
        event: $voice_agent_step[0].event,
        label: $voice_agent_step[0].label,
        source: $voice_agent_step[0].source,
        task: $voice_agent_step[0].task,
        tool: $voice_agent_step[0].tool,
        step_index: $voice_agent_step[0].step_index,
        step_total: $voice_agent_step[0].step_total,
        ttl_ms: $voice_agent_step[0].ttl_ms,
        reply_event: $voice_agent_reply[0].event,
        reply_text: $voice_agent_reply[0].text
      },
      context: {
        width: $unix_context[0].frame.envelope.width,
        height: $unix_context[0].frame.envelope.height,
        display_x: $unix_context[0].frame.envelope.display_x,
        display_y: $unix_context[0].frame.envelope.display_y,
        frame_origin_x: $unix_context[0].frame.envelope.frame_origin_x,
        frame_origin_y: $unix_context[0].frame.envelope.frame_origin_y,
        window_count: ($unix_context[0].desktop.windows | length)
      },
      pause_state: $unix_pause[0].safety_state,
      resume_state: $unix_resume[0].safety_state
    }
  }' > "$PROOF"

jq -e '.ok == true' "$PROOF" >/dev/null

echo "$OUT_DIR"
