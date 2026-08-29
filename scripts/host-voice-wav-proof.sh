#!/usr/bin/env bash
set -euo pipefail

export CUA_DEV_HTTP_TOKEN_OVERRIDE=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v afconvert >/dev/null
command -v curl >/dev/null
command -v jq >/dev/null
command -v perl >/dev/null
command -v say >/dev/null

RUN_ID="$(date +%s)"
PROFILE="${CUA_VOICE_WAV_PROOF_PROFILE:-host-voice-wav-proof-$RUN_ID}"
ADDR="${CUA_VOICE_WAV_PROOF_ADDR:-127.0.0.1:9881}"
TOKEN="${CUA_HTTP_TOKEN:-host-voice-wav-proof-token-$RUN_ID}"
OUT_DIR="${CUA_VOICE_WAV_PROOF_OUT_DIR:-artifacts/cua/voice-wav-proof-$RUN_ID}"
STT_MODEL="${CUA_VOICE_WAV_PROOF_STT_MODEL:-openai/whisper-1}"
PLANNER_MODEL="${CUA_VOICE_WAV_PROOF_PLANNER_MODEL:-gemini-3-flash-preview}"
BUDGET_MS="${CUA_VOICE_WAV_PROOF_BUDGET_MS:-60000}"
PHRASE="${CUA_VOICE_WAV_PROOF_PHRASE:-pause}"
EXPECT_TRANSCRIPT="${CUA_VOICE_WAV_PROOF_EXPECT_TRANSCRIPT:-pause}"
EXPECT_TOOL="${CUA_VOICE_WAV_PROOF_EXPECT_TOOL:-Command parser}"
EXPECT_DISPATCH_CONTAINS="${CUA_VOICE_WAV_PROOF_EXPECT_DISPATCH_CONTAINS-Pause}"
EXPECT_REPLY_CONTAINS="${CUA_VOICE_WAV_PROOF_EXPECT_REPLY_CONTAINS-paused}"
EXPECT_REPLY_CONTAINS_2="${CUA_VOICE_WAV_PROOF_EXPECT_REPLY_CONTAINS_2:-}"
EXPECT_SAFETY_STATE="${CUA_VOICE_WAV_PROOF_EXPECT_SAFETY_STATE:-paused}"
TRACE_DIR="$OUT_DIR/trace"
AIFF="$OUT_DIR/input.aiff"
WAV="$OUT_DIR/input.wav"
EVENTS="$OUT_DIR/events.jsonl"
DAEMON_EVENTS="$OUT_DIR/daemon-events.json"
STATUS="$OUT_DIR/status.json"
PROOF="$OUT_DIR/proof.json"

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

mkdir -p "$OUT_DIR"
say -o "$AIFF" "$PHRASE"
afconvert -f WAVE -d LEI16@16000 "$AIFF" "$WAV"

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

START_MS="$(perl -MTime::HiRes=time -e 'printf "%.0f\n", time() * 1000')"
CUA_HTTP_TOKEN="$TOKEN" "$VOICE_BIN_PATH" \
  --profile "$PROFILE" \
  --stt-model "$STT_MODEL" \
  --planner-model "$PLANNER_MODEL" \
  --once-wav "$WAV" > "$EVENTS"
END_MS="$(perl -MTime::HiRes=time -e 'printf "%.0f\n", time() * 1000')"
VOICE_ELAPSED_MS="$((END_MS - START_MS))"

sleep 0.2

CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" \
  --server-addr "$ADDR" \
  --profile "$PROFILE" \
  events --json > "$DAEMON_EVENTS"

CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" \
  --server-addr "$ADDR" \
  --profile "$PROFILE" \
  status --json > "$STATUS"

jq -s -e '
  def optional_contains($needle): (($needle | length) == 0) or contains($needle);
  def idx($name): map(.event) | index($name);
  any(.event == "transcribing") and
  any(.event == "transcript" and (.text | ascii_downcase | contains($expect_transcript))) and
  any(.event == "planning" and .tool == $expect_tool) and
  any(.event == "dispatching" and (.action | optional_contains($expect_dispatch))) and
  any(.event == "reply" and (.text | ascii_downcase | optional_contains($expect_reply)) and (.text | ascii_downcase | optional_contains($expect_reply_2))) and
  (idx("transcribing") < idx("transcript")) and
  (idx("transcript") < idx("planning")) and
  (idx("planning") < idx("dispatching")) and
  (idx("dispatching") < idx("reply")) and
  any(.event == "metric" and .name == "stt_preflight_overlap_ms") and
  any(.event == "metric" and .name == "stt_ms") and
  any(.event == "metric" and .name == "context_prefetch_aborted_ms") and
  any(.event == "metric" and .name == "plan_ms") and
  any(.event == "metric" and .name == "dispatch_ms") and
  any(.event == "metric" and .name == "turn_total_ms")
' \
  --arg expect_transcript "$(printf '%s' "$EXPECT_TRANSCRIPT" | tr '[:upper:]' '[:lower:]')" \
  --arg expect_tool "$EXPECT_TOOL" \
  --arg expect_dispatch "$EXPECT_DISPATCH_CONTAINS" \
  --arg expect_reply "$(printf '%s' "$EXPECT_REPLY_CONTAINS" | tr '[:upper:]' '[:lower:]')" \
  --arg expect_reply_2 "$(printf '%s' "$EXPECT_REPLY_CONTAINS_2" | tr '[:upper:]' '[:lower:]')" \
  "$EVENTS" >/dev/null

jq -e '
  any(.kind == "ui_step" and .data.source == "voice" and (.data.label | contains("transcript:"))) and
  any(.kind == "ui_step" and .data.source == "voice" and (.data.label | contains("dispatch:"))) and
  any(.kind == "ui_step" and .data.source == "voice" and (.data.label | contains("reply:")))
' "$DAEMON_EVENTS" >/dev/null

jq -e '
  .safety_state == $expect_safety_state
' --arg expect_safety_state "$EXPECT_SAFETY_STATE" "$STATUS" >/dev/null

jq -n \
  --arg profile "$PROFILE" \
  --arg phrase "$PHRASE" \
  --arg stt_model "$STT_MODEL" \
  --arg planner_model "$PLANNER_MODEL" \
  --arg wav "$WAV" \
  --argjson elapsed_ms "$VOICE_ELAPSED_MS" \
  --argjson budget_ms "$BUDGET_MS" \
  --slurpfile events "$EVENTS" \
  --slurpfile daemon_events "$DAEMON_EVENTS" \
  --slurpfile status "$STATUS" \
  '{
    schema_version: "cua.voice_proof.v1",
    profile: $profile,
    phrase: $phrase,
    stt_model: $stt_model,
    planner_model: $planner_model,
    wav: $wav,
    elapsed_ms: $elapsed_ms,
    budget_ms: $budget_ms,
    within_budget: ($elapsed_ms <= $budget_ms),
    events: ($events | map(.event)),
    daemon_voice_steps: ($daemon_events[0] | map(select(.kind == "ui_step" and .data.source == "voice") | .data.label)),
    metrics: ($events | map(select(.event == "metric")) | map({(.name): .ms}) | add),
    transcript: (($events | map(select(.event == "transcript")) | first).text),
    dispatch: (($events | map(select(.event == "dispatching")) | first).action),
    reply: (($events | map(select(.event == "reply")) | first).text),
    safety_state: $status[0].safety_state,
    active_profile: $status[0].active_profile
  }' > "$PROOF"

jq -e '
  .within_budget == true
' "$PROOF" >/dev/null

echo "$OUT_DIR"
