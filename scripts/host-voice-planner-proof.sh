#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v afconvert >/dev/null
command -v curl >/dev/null
command -v jq >/dev/null
command -v perl >/dev/null
command -v say >/dev/null

if [[ -z "${OPENROUTER_API_KEY:-}" && -f .env ]]; then
  while IFS='=' read -r key value; do
    key="${key#export }"
    if [[ "$key" == "OPENROUTER_API_KEY" && -n "${value:-}" ]]; then
      value="${value%$'\r'}"
      value="${value%\"}"
      value="${value#\"}"
      value="${value%\'}"
      value="${value#\'}"
      export OPENROUTER_API_KEY="$value"
      break
    fi
  done < .env
fi

if [[ -z "${OPENROUTER_API_KEY:-}" ]]; then
  echo "OPENROUTER_API_KEY is required for host voice planner proof" >&2
  exit 1
fi

RUN_ID="$(date +%s)"
PROFILE="${CUA_VOICE_PLANNER_PROOF_PROFILE:-host-voice-planner-proof-$RUN_ID}"
ADDR="${CUA_VOICE_PLANNER_PROOF_ADDR:-127.0.0.1:9882}"
TOKEN="${CUA_HTTP_TOKEN:-host-voice-planner-proof-token-$RUN_ID}"
OUT_DIR="${CUA_VOICE_PLANNER_PROOF_OUT_DIR:-artifacts/cua/voice-planner-proof-$RUN_ID}"
STT_MODEL="${CUA_VOICE_PLANNER_PROOF_STT_MODEL:-openai/whisper-1}"
PLANNER_MODEL="${CUA_VOICE_PLANNER_PROOF_MODEL:-openai/gpt-5.4-mini}"
BUDGET_MS="${CUA_VOICE_PLANNER_PROOF_BUDGET_MS:-30000}"
PHRASE="${CUA_VOICE_PLANNER_PROOF_PHRASE:-describe the current desktop briefly without taking action}"
EXPECT_TRANSCRIPT="${CUA_VOICE_PLANNER_PROOF_EXPECT_TRANSCRIPT:-desktop}"
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
CUA_HTTP_TOKEN="$TOKEN" OPENROUTER_API_KEY="$OPENROUTER_API_KEY" "$VOICE_BIN_PATH" \
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
  any(.event == "transcribing") and
  any(.event == "transcript" and (.text | ascii_downcase | contains($expect_transcript))) and
  any(.event == "planning" and .tool == "OpenRouter Vision") and
  (map(select(.event == "dispatching")) | length == 0) and
  any(.event == "reply" and ((.text // "") | length > 0)) and
  any(.event == "metric" and .name == "stt_preflight_overlap_ms") and
  any(.event == "metric" and .name == "context_stt_overlap_ms") and
  any(.event == "metric" and .name == "context_prefetch_ms") and
  any(.event == "metric" and .name == "context_wait_ms") and
  any(.event == "metric" and .name == "plan_ms" and .ms > 0) and
  any(.event == "metric" and .name == "turn_total_ms")
' \
  --arg expect_transcript "$(printf '%s' "$EXPECT_TRANSCRIPT" | tr '[:upper:]' '[:lower:]')" \
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
