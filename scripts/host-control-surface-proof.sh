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
HTTP_OBSERVE="$OUT_DIR/http-observe.json"
HTTP_CONTEXT="$OUT_DIR/http-context.json"
HTTP_SCREENSHOT="$OUT_DIR/http-screenshot.json"
CLI_STATUS="$OUT_DIR/cli-status.json"
CLI_MANIFEST="$OUT_DIR/cli-manifest.json"
CLI_METRICS="$OUT_DIR/cli-metrics.json"
CLI_EVENTS="$OUT_DIR/cli-events.json"
CLI_OBSERVE="$OUT_DIR/cli-observe.json"
CLI_CONTEXT="$OUT_DIR/cli-context.json"
CLI_PROFILE="$OUT_DIR/cli-profile.json"
CLI_PAUSE="$OUT_DIR/cli-pause.json"
CLI_RESUME="$OUT_DIR/cli-resume.json"
CLI_SCREENSHOT_JSON="$OUT_DIR/cli-screenshot.json"
CLI_SCREENSHOT_PNG="$OUT_DIR/cli-screenshot.png"
UNIX_STATUS="$OUT_DIR/unix-status.json"
UNIX_MANIFEST="$OUT_DIR/unix-manifest.json"
UNIX_METRICS="$OUT_DIR/unix-metrics.json"
UNIX_EVENTS="$OUT_DIR/unix-events.json"
UNIX_CONTEXT="$OUT_DIR/unix-context.json"
UNIX_PAUSE="$OUT_DIR/unix-pause.json"
UNIX_RESUME="$OUT_DIR/unix-resume.json"
PROOF="$OUT_DIR/proof.json"

cargo build -p cua

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

curl -fsS -H "authorization: Bearer $TOKEN" "http://$ADDR/status" > "$HTTP_STATUS"
curl -fsS -H "authorization: Bearer $TOKEN" "http://$ADDR/manifest" > "$HTTP_MANIFEST"
curl -fsS -H "authorization: Bearer $TOKEN" "http://$ADDR/metrics" > "$HTTP_METRICS"
curl -fsS -H "authorization: Bearer $TOKEN" "http://$ADDR/events" > "$HTTP_EVENTS"
curl -fsS -H "authorization: Bearer $TOKEN" "http://$ADDR/observe/desktop" > "$HTTP_OBSERVE"
curl -fsS \
  -H "authorization: Bearer $TOKEN" \
  -H "content-type: application/json" \
  -d '{"max_width":640,"encoding":"png","force_fresh":true,"include_bytes":false}' \
  "http://$ADDR/context/snapshot" > "$HTTP_CONTEXT"
curl -fsS \
  -H "authorization: Bearer $TOKEN" \
  -H "content-type: application/json" \
  -d '{"max_width":640,"encoding":"png","force_fresh":true,"include_bytes":false}' \
  "http://$ADDR/capture/screenshot" > "$HTTP_SCREENSHOT"

CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --server-addr "$ADDR" --profile "$PROFILE" status --json > "$CLI_STATUS"
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --server-addr "$ADDR" --profile "$PROFILE" manifest --json > "$CLI_MANIFEST"
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --server-addr "$ADDR" --profile "$PROFILE" metrics --json > "$CLI_METRICS"
CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" --server-addr "$ADDR" --profile "$PROFILE" events --json > "$CLI_EVENTS"
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

unix_call "status" '{}' "$UNIX_STATUS"
unix_call "manifest" '{}' "$UNIX_MANIFEST"
unix_call "metrics" '{}' "$UNIX_METRICS"
unix_call "events.snapshot" '{}' "$UNIX_EVENTS"
unix_call "context.snapshot" '{"max_width":640,"encoding":"png","force_fresh":true,"include_bytes":false}' "$UNIX_CONTEXT"
unix_call "control.pause" '{}' "$UNIX_PAUSE"
unix_call "control.resume" '{}' "$UNIX_RESUME"

jq -e '.schema_version == "cua.v1" and .active_profile == $profile' \
  --arg profile "$PROFILE" "$HTTP_STATUS" >/dev/null
jq -e '(.public_surfaces | index("cli")) and (.public_surfaces | index("local_http")) and (.endpoints | index("POST /context/snapshot"))' \
  "$HTTP_MANIFEST" >/dev/null
jq -e '.schema_version == "cua.v1" and (.histograms | type) == "array" and (.counters | type) == "object"' \
  "$HTTP_METRICS" >/dev/null
jq -e 'type == "array" and length >= 1 and .[0].kind == "daemon_started"' \
  "$HTTP_EVENTS" >/dev/null
jq -e '(.displays | length) >= 1 and (.windows | type) == "array" and (.cursor.visible | type) == "boolean"' \
  "$HTTP_OBSERVE" >/dev/null
jq -e '.frame.envelope.encoding == "png" and .frame.envelope.width > 0 and (.desktop.displays | length) >= 1 and (.desktop.windows | type) == "array"' \
  "$HTTP_CONTEXT" >/dev/null
jq -e '.envelope.encoding == "png" and .envelope.width > 0 and .envelope.height > 0 and (.envelope.sha256 | length) > 0' \
  "$HTTP_SCREENSHOT" >/dev/null
jq -e '.schema_version == "cua.v1" and .active_profile == $profile' \
  --arg profile "$PROFILE" "$CLI_STATUS" >/dev/null
jq -e '(.public_surfaces | index("cli")) and (.public_surfaces | index("local_http")) and (.commands | index("cua context --json"))' \
  "$CLI_MANIFEST" >/dev/null
jq -e '.schema_version == "cua.v1" and (.histograms | type) == "array" and (.counters | type) == "object"' \
  "$CLI_METRICS" >/dev/null
jq -e 'type == "array" and length >= 1 and .[0].kind == "daemon_started"' \
  "$CLI_EVENTS" >/dev/null
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
jq -e '.schema_version == "cua.v1" and .active_profile == $profile' \
  --arg profile "$PROFILE" "$UNIX_STATUS" >/dev/null
jq -e '(.public_surfaces | index("local_unix_socket")) and (.endpoints | index("POST /context/snapshot"))' \
  "$UNIX_MANIFEST" >/dev/null
jq -e '.schema_version == "cua.v1" and (.histograms | type) == "array" and (.counters | type) == "object"' \
  "$UNIX_METRICS" >/dev/null
jq -e 'type == "array" and length >= 1 and .[0].kind == "daemon_started"' \
  "$UNIX_EVENTS" >/dev/null
jq -e '.frame.envelope.encoding == "png" and .frame.envelope.width > 0 and (.desktop.displays | length) >= 1 and (.desktop.windows | type) == "array"' \
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
  --slurpfile http_observe "$HTTP_OBSERVE" \
  --slurpfile http_context "$HTTP_CONTEXT" \
  --slurpfile http_screenshot "$HTTP_SCREENSHOT" \
  --slurpfile cli_status "$CLI_STATUS" \
  --slurpfile cli_manifest "$CLI_MANIFEST" \
  --slurpfile cli_metrics "$CLI_METRICS" \
  --slurpfile cli_events "$CLI_EVENTS" \
  --slurpfile cli_observe "$CLI_OBSERVE" \
  --slurpfile cli_context "$CLI_CONTEXT" \
  --slurpfile cli_profile "$CLI_PROFILE" \
  --slurpfile cli_pause "$CLI_PAUSE" \
  --slurpfile cli_resume "$CLI_RESUME" \
  --slurpfile cli_screenshot_json "$CLI_SCREENSHOT_JSON" \
  --slurpfile unix_status "$UNIX_STATUS" \
  --slurpfile unix_manifest "$UNIX_MANIFEST" \
  --slurpfile unix_metrics "$UNIX_METRICS" \
  --slurpfile unix_events "$UNIX_EVENTS" \
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
      display_count: ($http_observe[0].displays | length),
      window_count: ($http_observe[0].windows | length),
      context: {
        width: $http_context[0].frame.envelope.width,
        height: $http_context[0].frame.envelope.height,
        window_count: ($http_context[0].desktop.windows | length)
      },
      screenshot: {
        width: $http_screenshot[0].envelope.width,
        height: $http_screenshot[0].envelope.height,
        sha256: $http_screenshot[0].envelope.sha256
      }
    },
    cli: {
      active_profile: $cli_status[0].active_profile,
      command_count: ($cli_manifest[0].commands | length),
      histogram_count: ($cli_metrics[0].histograms | length),
      event_count: ($cli_events[0] | length),
      profile_status: $cli_profile[0].active_profile.name,
      display_count: ($cli_observe[0].displays | length),
      window_count: ($cli_observe[0].windows | length),
      context: {
        width: $cli_context[0].frame.envelope.width,
        height: $cli_context[0].frame.envelope.height,
        window_count: ($cli_context[0].desktop.windows | length)
      },
      pause_state: $cli_pause[0].safety_state,
      resume_state: $cli_resume[0].safety_state,
      screenshot_path: $cli_screenshot,
      screenshot: {
        width: $cli_screenshot_json[0].width,
        height: $cli_screenshot_json[0].height,
        sha256: $cli_screenshot_json[0].sha256
      }
    },
    unix: {
      active_profile: $unix_status[0].active_profile,
      endpoint_count: ($unix_manifest[0].endpoints | length),
      histogram_count: ($unix_metrics[0].histograms | length),
      event_count: ($unix_events[0] | length),
      context: {
        width: $unix_context[0].frame.envelope.width,
        height: $unix_context[0].frame.envelope.height,
        window_count: ($unix_context[0].desktop.windows | length)
      },
      pause_state: $unix_pause[0].safety_state,
      resume_state: $unix_resume[0].safety_state
    }
  }' > "$PROOF"

jq -e '.ok == true' "$PROOF" >/dev/null

echo "$OUT_DIR"
