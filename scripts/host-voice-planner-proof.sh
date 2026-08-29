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
PROFILE="${CUA_VOICE_PLANNER_PROOF_PROFILE:-host-voice-planner-proof-$RUN_ID}"
ADDR="${CUA_VOICE_PLANNER_PROOF_ADDR:-127.0.0.1:9882}"
TOKEN="${CUA_HTTP_TOKEN:-host-voice-planner-proof-token-$RUN_ID}"
OUT_DIR="${CUA_VOICE_PLANNER_PROOF_OUT_DIR:-artifacts/cua/voice-planner-proof-$RUN_ID}"
STT_MODEL="${CUA_VOICE_PLANNER_PROOF_STT_MODEL:-openai/whisper-1}"
PLANNER_MODEL="${CUA_VOICE_PLANNER_PROOF_MODEL:-gemini-3-flash-preview}"
BUDGET_MS="${CUA_VOICE_PLANNER_PROOF_BUDGET_MS:-30000}"
PHRASE="${CUA_VOICE_PLANNER_PROOF_PHRASE:-describe the current desktop briefly without taking action}"
EXPECT_TRANSCRIPT="${CUA_VOICE_PLANNER_PROOF_EXPECT_TRANSCRIPT:-desktop}"
EXPECT_PLANNER_TOOL="${CUA_VOICE_PLANNER_PROOF_EXPECT_TOOL:-Gemini Vision}"
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

env_key_available() {
  local name="$1"
  [[ -n "${!name:-}" ]] && return 0
  grep -Eq "^[[:space:]]*(export[[:space:]]+)?${name}=" "${CUA_HOME:-$HOME/.cua}/config/env" 2>/dev/null && return 0
  return 1
}

planner_key_available() {
  if [[ "$PLANNER_MODEL" == gemini-* ]]; then
    env_key_available GEMINI_API_KEY || env_key_available GOOGLE_API_KEY
  else
    env_key_available OPENROUTER_API_KEY
  fi
}

planner_key_name() {
  if [[ "$PLANNER_MODEL" == gemini-* ]]; then
    printf 'GEMINI_API_KEY or GOOGLE_API_KEY'
  else
    printf 'OPENROUTER_API_KEY'
  fi
}

if ! planner_key_available; then
  echo "$(planner_key_name) is required for host voice planner proof with planner model $PLANNER_MODEL; set it in the environment or ~/.cua/config/env before launching cua" >&2
  exit 1
fi

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
  def idx($name): map(.event) | index($name);
  any(.event == "transcribing") and
  any(.event == "transcript" and (.text | ascii_downcase | contains($expect_transcript))) and
  any(.event == "planning" and .tool == $expect_planner_tool) and
  (map(select(.event == "dispatching")) | length == 0) and
  any(.event == "reply" and ((.text // "") | length > 0)) and
  (idx("transcribing") < idx("transcript")) and
  (idx("transcript") < idx("planning")) and
  (idx("planning") < idx("reply")) and
  any(.event == "metric" and .name == "stt_preflight_overlap_ms") and
  any(.event == "metric" and .name == "stt_ms") and
  any(.event == "metric" and .name == "context_prefetch_ms") and
  any(.event == "metric" and .name == "context_wait_ms") and
  any(.event == "metric" and .name == "plan_ms" and .ms > 0) and
  any(.event == "metric" and .name == "turn_total_ms")
' \
  --arg expect_transcript "$(printf '%s' "$EXPECT_TRANSCRIPT" | tr '[:upper:]' '[:lower:]')" \
  --arg expect_planner_tool "$EXPECT_PLANNER_TOOL" \
  "$EVENTS" >/dev/null

jq -e '
  any(.kind == "ui_step" and .data.source == "voice" and (.data.label | contains("transcript:"))) and
  any(.kind == "ui_step" and .data.source == "voice" and (.data.label | contains("planning from screen context"))) and
  any(.kind == "ui_step" and .data.source == "voice" and (.data.label | contains("reply:"))) and
  (map(select(.kind == "ui_step" and .data.source == "voice" and (.data.label | contains("dispatch:")))) | length == 0)
' "$DAEMON_EVENTS" >/dev/null

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
    schema_version: "cua.voice_planner_proof.v1",
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
    reply: (($events | map(select(.event == "reply")) | first).text),
    safety_state: $status[0].safety_state,
    active_profile: $status[0].active_profile
  }' > "$PROOF"

jq -e '.within_budget == true' "$PROOF" >/dev/null

echo "$OUT_DIR"
