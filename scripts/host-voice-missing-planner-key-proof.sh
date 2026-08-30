#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v jq >/dev/null
command -v perl >/dev/null
command -v sqlite3 >/dev/null

RUN_ID="$(date +%s)"
SUFFIX="${RUN_ID: -5}"
PROFILE="${CUA_VOICE_MISSING_KEY_PROFILE:-qmk$SUFFIX}"
OUT_DIR="${CUA_VOICE_MISSING_KEY_OUT_DIR:-artifacts/cua/voice-missing-key-$RUN_ID}"
CUA_HOME_DIR="${CUA_VOICE_MISSING_KEY_HOME:-}"
PLANNER_MODEL="${CUA_VOICE_MISSING_KEY_MODEL:-gemini-3.7-flash}"
BUDGET_MS="${CUA_VOICE_MISSING_KEY_BUDGET_MS:-60000}"
TRANSCRIPT="${CUA_VOICE_MISSING_KEY_TRANSCRIPT:-Using Aegis headless only, search the web for the official SQLite foreign key documentation and report the verified page title.}"
EVENTS="$OUT_DIR/events.jsonl"
PROOF="$OUT_DIR/proof.json"

cargo build -p cua-voice

if [[ -n "${CUA_VOICE_BIN:-}" ]]; then
  VOICE_BIN_PATH="$CUA_VOICE_BIN"
elif [[ -x target/debug/cua-voice ]]; then
  VOICE_BIN_PATH="target/debug/cua-voice"
else
  VOICE_BIN_PATH="$(find target -path '*/debug/cua-voice' -type f 2>/dev/null | head -n 1)"
fi

if [[ -z "$VOICE_BIN_PATH" || ! -x "$VOICE_BIN_PATH" ]]; then
  echo "cua-voice binary not found" >&2
  exit 1
fi

if [[ "$PLANNER_MODEL" != gemini-* ]]; then
  echo "missing-key proof requires a direct Gemini planner model, got $PLANNER_MODEL" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"
if [[ -z "$CUA_HOME_DIR" ]]; then
  CUA_HOME_DIR="$(mktemp -d /tmp/cuamk.XXXXXX)"
else
  mkdir -p "$CUA_HOME_DIR"
fi
mkdir -p "$CUA_HOME_DIR/config"
ENV_FILE="$CUA_HOME_DIR/config/env"
: > "$ENV_FILE"
TRACE="$CUA_HOME_DIR/profiles/$PROFILE/traces/voice.jsonl"
CHAT_DB="$CUA_HOME_DIR/profiles/$PROFILE/chat.db"

START_MS="$(perl -MTime::HiRes=time -e 'printf "%.0f\n", time() * 1000')"
env -u GEMINI_API_KEY -u GOOGLE_API_KEY -u OPENROUTER_API_KEY \
  CUA_HOME="$CUA_HOME_DIR" \
  CUA_ENV_FILE="$ENV_FILE" \
  CUA_VOICE_DEBUG_TRACE=true \
  "$VOICE_BIN_PATH" \
    --profile "$PROFILE" \
    --headless \
    --planner-model "$PLANNER_MODEL" \
    --once-agent-reply-wait-ms "$BUDGET_MS" \
    --once-transcript "$TRANSCRIPT" > "$EVENTS"
END_MS="$(perl -MTime::HiRes=time -e 'printf "%.0f\n", time() * 1000')"
ELAPSED_MS="$((END_MS - START_MS))"

if [[ ! -f "$TRACE" ]]; then
  echo "voice trace not found at $TRACE" >&2
  exit 1
fi
if [[ ! -f "$CHAT_DB" ]]; then
  echo "chat database not found at $CHAT_DB" >&2
  exit 1
fi

jq -s -e '
  def idx($name): map(.event) | index($name);
  any(.event == "armed") and
  any(.event == "transcript") and
  any(.event == "planning" and .tool == "Desktop context") and
  any(.event == "planning" and .tool == "Command parser") and
  any(.event == "dispatching" and (.action | contains("Aegis"))) and
  any(.event == "planning" and .tool == "Reobserving") and
  any(.event == "planning" and (.tool | contains("Gemini repair 2/n"))) and
  any(.event == "metric" and .name == "context_wait_ms") and
  any(.event == "metric" and .name == "context_prefetch_ms") and
  any(.event == "metric" and .name == "dispatch_ms") and
  any(.event == "metric" and .name == "reobserve_ms") and
  any(.event == "metric" and .name == "plan_ms") and
  any(.event == "reply" and ((.text // "") | contains("GEMINI_API_KEY or GOOGLE_API_KEY is required")) and ((.text // "") | contains("action attempt")) and ((.text // "") | contains("last attempted"))) and
  any(.event == "metric" and .name == "turn_total_ms") and
  (idx("transcript") < idx("dispatching")) and
  (idx("dispatching") < idx("reply")) and
  (map(select(.event == "reply")) | all((.text // "") | test("\\b[Cc]onfirmed\\b") | not))
' "$EVENTS" >/dev/null

jq -s -e '
  any(.event == "planning_start") and
  any(.event == "planner_hints") and
  any(.event == "context_result") and
  any(.event == "agent_context_result") and
  any(.event == "agent_loop_start" and .data.budget.kind == "unbounded") and
  any(.event == "planning_pre_model_bootstrap") and
  any(.event == "dispatch_result" and (.data.result.effect | IN("confirmed", "failed", "refused", "partial", "unverifiable"))) and
  any(.event == "agent_attempt_outcome" and .data.has_action == true and .data.should_replan == true) and
  any(.event == "agent_reobserve_result") and
  any(.event == "planning_error" and .data.reason == "planning_credentials_missing") and
  any(.event == "agent_loop_stop" and .data.attempts > 0 and .data.final_effect == "failed") and
  any(.event == "reply" and ((.data.text // "") | contains("GEMINI_API_KEY or GOOGLE_API_KEY is required")) and ((.data.text // "") | contains("action attempt")) and ((.data.text // "") | contains("last attempted"))) and
  any(.event == "memory_persisted") and
  any(.event == "turn_complete") and
  (map(select(.event == "reply")) | all((.data.text // "") | test("\\b[Cc]onfirmed\\b") | not))
' "$TRACE" >/dev/null

CHAT_ROWS="$(
  sqlite3 "$CHAT_DB" \
    "select count(*) from chat_messages where role in ('user', 'assistant');"
)"
if [[ "$CHAT_ROWS" -lt 2 ]]; then
  echo "expected persisted user and assistant chat rows, got $CHAT_ROWS" >&2
  exit 1
fi

jq -n \
  --arg profile "$PROFILE" \
  --arg planner_model "$PLANNER_MODEL" \
  --arg transcript "$TRANSCRIPT" \
  --arg events_path "$EVENTS" \
  --arg trace_path "$TRACE" \
  --arg chat_db "$CHAT_DB" \
  --argjson elapsed_ms "$ELAPSED_MS" \
  --argjson budget_ms "$BUDGET_MS" \
  --slurpfile events "$EVENTS" \
  --slurpfile trace "$TRACE" \
  '{
    schema_version: "cua.voice_missing_planner_key_proof.v1",
    ok: true,
    profile: $profile,
    planner_model: $planner_model,
    transcript: $transcript,
    elapsed_ms: $elapsed_ms,
    budget_ms: $budget_ms,
    within_budget: ($elapsed_ms <= $budget_ms),
    events_path: $events_path,
    trace_path: $trace_path,
    chat_db: $chat_db,
    events: ($events | map(.event)),
    reply: (($events | map(select(.event == "reply")) | last).text),
    trace_stop: (($trace | map(select(.event == "agent_loop_stop")) | last).data),
    memory_persisted: ($trace | any(.event == "memory_persisted"))
  }' > "$PROOF"

jq -e '.ok == true and .within_budget == true' "$PROOF" >/dev/null

echo "$OUT_DIR"
