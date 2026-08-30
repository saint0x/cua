#!/usr/bin/env bash
set -euo pipefail

export CUA_DEV_HTTP_TOKEN_OVERRIDE=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v curl >/dev/null
command -v jq >/dev/null
command -v python3 >/dev/null

RUN_ID="$(date +%s)"
PROFILE="${CUA_VOICE_LOCAL_PLANNER_PROFILE:-host-voice-local-planner-$RUN_ID}"
ADDR="${CUA_VOICE_LOCAL_PLANNER_ADDR:-127.0.0.1:9894}"
PLANNER_ADDR="${CUA_VOICE_LOCAL_PLANNER_MODEL_ADDR:-127.0.0.1:9895}"
TOKEN="${CUA_HTTP_TOKEN:-host-voice-local-planner-token-$RUN_ID}"
OUT_DIR="${CUA_VOICE_LOCAL_PLANNER_OUT_DIR:-artifacts/cua/voice-local-planner-$RUN_ID}"
CUA_HOME_DIR="${CUA_VOICE_LOCAL_PLANNER_HOME:-}"
SCENARIO="${CUA_VOICE_LOCAL_PLANNER_SCENARIO:-missing-readback}"
EVENTS="$OUT_DIR/events.jsonl"
DAEMON_EVENTS="$OUT_DIR/daemon-events.json"
STATUS="$OUT_DIR/status.json"
REQUESTS="$OUT_DIR/planner-requests.jsonl"
PROOF="$OUT_DIR/proof.json"
VOICE_TRACE="$OUT_DIR/voice.jsonl"
VOICE_STDERR="$OUT_DIR/voice.stderr"
case "$SCENARIO" in
  missing-readback | mismatch-readback | failed-action-repair | repeated-rejected-plan) ;;
  *)
    echo "unknown local planner proof scenario: $SCENARIO" >&2
    exit 1
    ;;
esac

TARGET_DIR="$OUT_DIR/work"
TARGET_FILE="$TARGET_DIR/value.txt"
EXPECTED="rlm loop verified stdout $RUN_ID"
WRONG_VALUE="wrong loop stdout $RUN_ID"
TRANSCRIPT="Using local shell only, create $TARGET_FILE containing exactly $EXPECTED, then read the file back to stdout, and report the exact final stdout."
if [[ "$SCENARIO" == "repeated-rejected-plan" ]]; then
  TRANSCRIPT="Using local shell only, create $TARGET_FILE containing exactly $EXPECTED, then read the file back to stdout, and report the exact final stdout."
elif [[ "$SCENARIO" == "failed-action-repair" ]]; then
  TRANSCRIPT="Using local shell only, first try to read $TARGET_FILE and observe the failure, then recover by creating it with exactly $EXPECTED, read the file back to stdout, and report the exact final stdout."
fi

cargo build -p cua -p cua-voice

if [[ -n "${CUA_BIN:-}" ]]; then
  CUA_BIN_PATH="$CUA_BIN"
elif [[ -x target/debug/cua ]]; then
  CUA_BIN_PATH="target/debug/cua"
else
  CUA_BIN_PATH="$(find target -path '*/debug/cua' -type f 2>/dev/null | head -n 1)"
fi

if [[ -n "${CUA_VOICE_BIN:-}" ]]; then
  VOICE_BIN_PATH="$CUA_VOICE_BIN"
elif [[ -x target/debug/cua-voice ]]; then
  VOICE_BIN_PATH="target/debug/cua-voice"
else
  VOICE_BIN_PATH="$(find target -path '*/debug/cua-voice' -type f 2>/dev/null | head -n 1)"
fi

if [[ -z "$CUA_BIN_PATH" || ! -x "$CUA_BIN_PATH" ]]; then
  echo "cua binary not found" >&2
  exit 1
fi
if [[ -z "$VOICE_BIN_PATH" || ! -x "$VOICE_BIN_PATH" ]]; then
  echo "cua-voice binary not found" >&2
  exit 1
fi

mkdir -p "$OUT_DIR" "$TARGET_DIR"
if [[ -z "$CUA_HOME_DIR" ]]; then
  CUA_HOME_DIR="$(mktemp -d /tmp/cualp.XXXXXX)"
else
  mkdir -p "$CUA_HOME_DIR"
fi
mkdir -p "$CUA_HOME_DIR/config"
: > "$CUA_HOME_DIR/config/env"

python3 - "$PLANNER_ADDR" "$REQUESTS" "$TARGET_FILE" "$EXPECTED" "$WRONG_VALUE" "$SCENARIO" <<'PY' &
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

host, port_text = sys.argv[1].rsplit(":", 1)
requests_path = sys.argv[2]
target_file = sys.argv[3]
expected = sys.argv[4]
wrong_value = sys.argv[5]
scenario = sys.argv[6]
counter = 0

def planner_content(index):
    if scenario == "repeated-rejected-plan":
        return {
            "response": f"The exact final stdout is {expected}.",
            "action": None,
        }
    if scenario == "failed-action-repair":
        if index == 1:
            return {
                "response": "Reading the missing file first.",
                "action": {
                    "kind": "shell_exec",
                    "command": f"cat {json.dumps(target_file)}",
                    "timeout_ms": 5000,
                },
            }
        return {
            "response": "Recovering and reading the file back.",
            "action": {
                "kind": "shell_exec",
                "command": f"mkdir -p {json.dumps(target_file.rsplit('/', 1)[0])} && printf %s {json.dumps(expected)} > {json.dumps(target_file)} && cat {json.dumps(target_file)}",
                "timeout_ms": 5000,
            },
        }
    if index == 1:
        if scenario == "mismatch-readback":
            return {
                "response": "Writing and reading an initial value.",
                "action": {
                    "kind": "shell_exec",
                    "command": f"mkdir -p {json.dumps(target_file.rsplit('/', 1)[0])} && printf %s {json.dumps(wrong_value)} > {json.dumps(target_file)} && cat {json.dumps(target_file)}",
                    "timeout_ms": 5000,
                },
            }
        return {
            "response": "Creating the file.",
            "action": {
                "kind": "shell_exec",
                "command": f"mkdir -p {json.dumps(target_file.rsplit('/', 1)[0])} && printf %s {json.dumps(expected)} > {json.dumps(target_file)}",
                "timeout_ms": 5000,
            },
        }
    if index == 2:
        return {
            "response": f"The exact final stdout is {expected}.",
            "action": None,
        }
    if index == 3:
        if scenario == "mismatch-readback":
            return {
                "response": "Repairing and reading the file back.",
                "action": {
                    "kind": "shell_exec",
                    "command": f"printf %s {json.dumps(expected)} > {json.dumps(target_file)} && cat {json.dumps(target_file)}",
                    "timeout_ms": 5000,
                },
            }
        return {
            "response": "Reading the file back.",
            "action": {
                "kind": "shell_exec",
                "command": f"cat {json.dumps(target_file)}",
                "timeout_ms": 5000,
            },
        }
    return {
        "response": f"{expected}",
        "action": None,
    }

class Handler(BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        return

    def do_POST(self):
        global counter
        length = int(self.headers.get("content-length", "0"))
        raw = self.rfile.read(length).decode("utf-8")
        try:
            body = json.loads(raw)
        except json.JSONDecodeError:
            body = {"raw": raw}
        counter += 1
        with open(requests_path, "a", encoding="utf-8") as handle:
            handle.write(json.dumps({"index": counter, "path": self.path, "body": body}) + "\n")
        content = json.dumps(planner_content(counter))
        response = {
            "id": f"local-planner-{counter}",
            "object": "chat.completion",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": content}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        }
        payload = json.dumps(response).encode("utf-8")
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

server = ThreadingHTTPServer((host, int(port_text)), Handler)
server.serve_forever()
PY
PLANNER_PID="$!"

CUA_HOME="$CUA_HOME_DIR" \
CUA_HTTP_TOKEN="$TOKEN" \
"$CUA_BIN_PATH" \
  --server-addr "$ADDR" \
  --profile "$PROFILE" \
  serve --addr "$ADDR" &
DAEMON_PID="$!"

cleanup() {
  kill "$DAEMON_PID" "$PLANNER_PID" >/dev/null 2>&1 || true
  wait "$DAEMON_PID" "$PLANNER_PID" >/dev/null 2>&1 || true
}
trap cleanup EXIT

for _ in $(seq 1 80); do
  if curl -fs "http://$ADDR/healthz" >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done
curl -fsS "http://$ADDR/healthz" >/dev/null

for _ in $(seq 1 80); do
  if python3 - "$PLANNER_ADDR" <<'PY' >/dev/null 2>&1
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
GEMINI_API_KEY="local-planner-test-key" \
CUA_VOICE_PLANNER_CHAT_COMPLETIONS_URL="http://$PLANNER_ADDR/v1/chat/completions" \
CUA_VOICE_TRACE_PATH="$VOICE_TRACE" \
CUA_AGENT_LOOP_MAX_ATTEMPTS="n" \
"$VOICE_BIN_PATH" \
  --profile "$PROFILE" \
  --debug-trace \
  --planner-model "gemini-3.7-flash" \
  --once-transcript "$TRANSCRIPT" > "$EVENTS" 2> "$VOICE_STDERR"
VOICE_EXIT=$?
set -e

sleep 0.2

CUA_HOME="$CUA_HOME_DIR" \
CUA_HTTP_TOKEN="$TOKEN" \
"$CUA_BIN_PATH" \
  --server-addr "$ADDR" \
  --profile "$PROFILE" \
  events --json > "$DAEMON_EVENTS"

CUA_HOME="$CUA_HOME_DIR" \
CUA_HTTP_TOKEN="$TOKEN" \
"$CUA_BIN_PATH" \
  --server-addr "$ADDR" \
  --profile "$PROFILE" \
  status --json > "$STATUS"

if [[ "$SCENARIO" != "repeated-rejected-plan" ]]; then
  test "$VOICE_EXIT" -eq 0
  test "$(cat "$TARGET_FILE")" = "$EXPECTED"
fi

if [[ "$SCENARIO" == "repeated-rejected-plan" ]]; then
  STALL_REASON="action_null_concrete_goal_without_evidence"
  test "$VOICE_EXIT" -ne 0
  grep -F "planning model repeated an unsupported plan 3 times without new action or evidence" "$VOICE_STDERR" >/dev/null
  jq -s -e '
    any(.event == "transcript") and
    any(.event == "planning") and
    (any(.event == "dispatching") | not) and
    (any(.event == "reply") | not)
  ' "$EVENTS" >/dev/null

  jq -s -e '
    length == 3 and
    all(.[]; .path == "/v1/chat/completions") and
    (.[1].body.messages[-1].content[0].text | contains("Prior attempts in this turn")) and
    (.[1].body.messages[-1].content[0].text | contains($stall_reason)) and
    (.[2].body.messages[-1].content[0].text | contains($stall_reason))
  ' --arg stall_reason "$STALL_REASON" "$REQUESTS" >/dev/null

  jq -s -e '
    any(.event == "agent_loop_start" and .data.budget.kind == "unbounded") and
    ([.[] | select(.event == "planning_rejected" and .data.reason == $stall_reason)] | length == 3) and
    any(.event == "planning_stalled" and .data.reason == $stall_reason and .data.repeat_count == 3)
  ' --arg stall_reason "$STALL_REASON" "$VOICE_TRACE" >/dev/null

  jq -n \
    --arg scenario "$SCENARIO" \
    --arg profile "$PROFILE" \
    --arg transcript "$TRANSCRIPT" \
    --arg expected "$EXPECTED" \
    --arg target_file "$TARGET_FILE" \
    --arg stall_reason "$STALL_REASON" \
    --arg stderr "$(cat "$VOICE_STDERR")" \
    --slurpfile events "$EVENTS" \
    --slurpfile requests "$REQUESTS" \
    --rawfile trace "$VOICE_TRACE" \
    '{
      schema_version: "cua.voice_local_planner_stall_proof.v1",
      ok: true,
      scenario: $scenario,
      profile: $profile,
      transcript: $transcript,
      expected: $expected,
      target_file: $target_file,
      stderr: $stderr,
      planner_requests: ($requests | length),
      stall_reason: $stall_reason,
      fake_success_suppressed: (($events | map(select(.event == "reply")) | length) == 0),
      dispatch_suppressed: (($events | map(select(.event == "dispatching")) | length) == 0),
      contextual_error_emitted: ($stderr | contains("planning model repeated an unsupported plan 3 times without new action or evidence")),
      planning_stalled: ($trace | contains("planning_stalled"))
    }' > "$PROOF"

  jq -e '
    .ok == true and
    .planner_requests == 3 and
    .fake_success_suppressed == true and
    .dispatch_suppressed == true and
    .contextual_error_emitted == true and
    .planning_stalled == true
  ' "$PROOF" >/dev/null

  echo "$OUT_DIR"
  exit 0
fi

if [[ "$SCENARIO" == "failed-action-repair" ]]; then
  test "$VOICE_EXIT" -eq 0
  test "$(cat "$TARGET_FILE")" = "$EXPECTED"

  jq -s -e \
    --arg expected "$EXPECTED" '
    def idx($name): map(.event) | index($name);
    any(.event == "transcript") and
    any(.event == "planning") and
    any(.event == "dispatching") and
    any(.event == "reply" and .text == $expected) and
    (idx("transcript") < idx("planning")) and
    (idx("planning") < idx("dispatching")) and
    (idx("dispatching") < idx("reply"))
  ' "$EVENTS" >/dev/null

  jq -s -e '
    length == 2 and
    (.[1].body.messages[-1].content[0].text | contains("Prior attempts in this turn")) and
    (.[1].body.messages[-1].content[0].text | contains("\"effect\": \"failed\"")) and
    (.[1].body.messages[-1].content[0].text | contains("No such file or directory"))
  ' "$REQUESTS" >/dev/null

  jq -s -e '
    any(.event == "agent_loop_start" and .data.budget.kind == "unbounded") and
    any(.event == "agent_attempt_outcome" and .data.effect == "failed" and .data.should_replan == true and .data.has_action == true) and
    any(.event == "agent_reobserve_start" and .data.reason == "repair_after_effect") and
    any(.event == "agent_loop_stop" and .data.attempts == 2 and .data.final_effect == "confirmed")
  ' "$VOICE_TRACE" >/dev/null

  jq -n \
    --arg scenario "$SCENARIO" \
    --arg profile "$PROFILE" \
    --arg transcript "$TRANSCRIPT" \
    --arg expected "$EXPECTED" \
    --arg target_file "$TARGET_FILE" \
    --slurpfile events "$EVENTS" \
    --slurpfile requests "$REQUESTS" \
    --rawfile trace "$VOICE_TRACE" \
    '{
      schema_version: "cua.voice_local_planner_failed_action_repair_proof.v1",
      ok: true,
      scenario: $scenario,
      profile: $profile,
      transcript: $transcript,
      expected: $expected,
      target_file: $target_file,
      final_reply: (($events | map(select(.event == "reply")) | last).text),
      planner_requests: ($requests | length),
      failed_evidence_reached_model: ($requests[1].body.messages[-1].content[0].text | contains("\"effect\": \"failed\"")),
      failed_error_reached_model: ($requests[1].body.messages[-1].content[0].text | contains("No such file or directory")),
      repaired_after_failure: ($trace | contains("\"effect\":\"failed\"") and contains("\"final_effect\":\"confirmed\""))
    }' > "$PROOF"

  jq -e '
    .ok == true and
    .final_reply == .expected and
    .planner_requests == 2 and
    .failed_evidence_reached_model == true and
    .failed_error_reached_model == true and
    .repaired_after_failure == true
  ' "$PROOF" >/dev/null

  echo "$OUT_DIR"
  exit 0
fi

case "$SCENARIO" in
  missing-readback)
    PARTIAL_REASON="shell_readback_missing_for_verified_output_goal"
    TRACE_PARTIAL_FIELD='"shell_readback_missing":true'
    ;;
  mismatch-readback)
    PARTIAL_REASON="shell_expected_final_stdout_not_observed"
    TRACE_PARTIAL_FIELD='"shell_expected_stdout_missing":true'
    ;;
esac

jq -s -e \
  --arg expected "$EXPECTED" '
  def idx($name): map(.event) | index($name);
  any(.event == "transcript") and
  any(.event == "planning") and
  any(.event == "dispatching") and
  any(.event == "reply" and .text == $expected) and
  (idx("transcript") < idx("planning")) and
  (idx("planning") < idx("dispatching")) and
  (idx("dispatching") < idx("reply")) and
  any(.event == "metric" and .name == "plan_ms" and .ms > 0) and
  any(.event == "metric" and .name == "turn_total_ms")
' "$EVENTS" >/dev/null

jq -e '
  any(.kind == "ui_step" and .data.source == "voice" and (.data.label | contains("transcript:"))) and
  any(.kind == "ui_step" and .data.source == "voice" and (.data.label | contains("planning: attempt"))) and
  any(.kind == "ui_step" and .data.source == "voice" and (.data.label | contains("dispatch:"))) and
  any(.kind == "ui_step" and .data.source == "voice" and (.data.label | contains("reply:")))
' "$DAEMON_EVENTS" >/dev/null

jq -s -e '
  def contains_reason($reason): .body.messages[-1].content[0].text | contains($reason);
  length == 3 and
  .[0].path == "/v1/chat/completions" and
  (.[1].body.messages[-1].content[0].text | contains("Prior attempts in this turn")) and
  (.[1].body.messages[-1].content[0].text | contains("\"effect\": \"partial\"")) and
  (.[1] | contains_reason($partial_reason)) and
  (.[1].body.messages[-1].content[0].text | contains("\"effect\": \"confirmed\"") | not) and
  (.[2].body.messages[-1].content[0].text | contains("action_null_concrete_goal_without_evidence")) and
  (.[2] | contains_reason($partial_reason))
' --arg partial_reason "$PARTIAL_REASON" "$REQUESTS" >/dev/null

jq -s -e '
  any(.event == "agent_loop_start" and .data.budget.kind == "unbounded") and
  any(.event == "agent_attempt_outcome" and .data.effect == "partial" and (. | tostring | contains($trace_partial_field))) and
  any(.event == "planning_rejected" and .data.reason == "action_null_concrete_goal_without_evidence") and
  any(.event == "agent_loop_stop" and .data.attempts == 3 and .data.final_effect == "confirmed")
' --arg trace_partial_field "$TRACE_PARTIAL_FIELD" "$VOICE_TRACE" >/dev/null

jq -n \
  --arg scenario "$SCENARIO" \
  --arg profile "$PROFILE" \
  --arg transcript "$TRANSCRIPT" \
  --arg expected "$EXPECTED" \
  --arg target_file "$TARGET_FILE" \
  --arg partial_reason "$PARTIAL_REASON" \
  --arg trace_partial_field "$TRACE_PARTIAL_FIELD" \
  --slurpfile events "$EVENTS" \
  --slurpfile daemon_events "$DAEMON_EVENTS" \
  --slurpfile requests "$REQUESTS" \
  --slurpfile status "$STATUS" \
  --rawfile trace "$VOICE_TRACE" \
  '{
    schema_version: "cua.voice_local_planner_loop_proof.v1",
    ok: true,
    scenario: $scenario,
    profile: $profile,
    transcript: $transcript,
    expected: $expected,
    target_file: $target_file,
    final_reply: (($events | map(select(.event == "reply")) | last).text),
    planner_requests: ($requests | length),
    request_paths: ($requests | map(.path)),
    repair_context_preserved_partial_evidence: (
      ($requests[1].body.messages[-1].content[0].text | contains("\"effect\": \"partial\"")) and
      ($requests[1].body.messages[-1].content[0].text | contains($partial_reason))
    ),
    model_visible_confirmed_leaked: ($requests[1].body.messages[-1].content[0].text | contains("\"effect\": \"confirmed\"")),
    premature_final_rejected: ($trace | contains("action_null_concrete_goal_without_evidence")),
    partial_repaired: ($trace | contains($trace_partial_field)),
    safety_state: $status[0].safety_state
  }' > "$PROOF"

jq -e '
  .ok == true and
  .final_reply == .expected and
  .planner_requests == 3 and
  .repair_context_preserved_partial_evidence == true and
  .model_visible_confirmed_leaked == false and
  .premature_final_rejected == true and
  .partial_repaired == true
' "$PROOF" >/dev/null

echo "$OUT_DIR"
