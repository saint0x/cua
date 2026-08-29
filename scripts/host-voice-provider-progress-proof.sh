#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v jq >/dev/null
command -v perl >/dev/null
command -v sqlite3 >/dev/null

RUN_ID="$(date +%s)"
SUFFIX="${RUN_ID: -5}"
PROFILE="${CUA_VOICE_PROVIDER_PROGRESS_PROFILE:-qpr$SUFFIX}"
OUT_DIR="${CUA_VOICE_PROVIDER_PROGRESS_OUT_DIR:-artifacts/cua/voice-provider-progress-$RUN_ID}"
CUA_HOME_DIR="${CUA_VOICE_PROVIDER_PROGRESS_HOME:-}"
ENV_FILE="${CUA_ENV_FILE:-$HOME/.cua/config/env}"
PLANNER_MODEL="${CUA_VOICE_PROVIDER_PROGRESS_MODEL:-openrouter/google/gemini-3.7-flash}"
BUDGET_MS="${CUA_VOICE_PROVIDER_PROGRESS_BUDGET_MS:-120000}"
TRANSCRIPT="${CUA_VOICE_PROVIDER_PROGRESS_TRANSCRIPT:-Using Aegis headless only, search the web for the official SQLite foreign key documentation and report the verified page title.}"
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

env_key_available() {
  local name="$1"
  [[ -n "${!name:-}" ]] && return 0
  grep -Eq "^[[:space:]]*(export[[:space:]]+)?${name}=" "$ENV_FILE" 2>/dev/null && return 0
  return 1
}

if [[ "$PLANNER_MODEL" != openrouter/* && "$PLANNER_MODEL" != google/* ]]; then
  echo "provider progress proof requires an OpenRouter-routed planner model, got $PLANNER_MODEL" >&2
  exit 1
fi

if ! env_key_available OPENROUTER_API_KEY; then
  echo "OPENROUTER_API_KEY is required in the environment or $ENV_FILE" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"
if [[ -z "$CUA_HOME_DIR" ]]; then
  CUA_HOME_DIR="$(mktemp -d /tmp/cuapr.XXXXXX)"
else
  mkdir -p "$CUA_HOME_DIR"
fi
TRACE="$CUA_HOME_DIR/profiles/$PROFILE/traces/voice.jsonl"
CHAT_DB="$CUA_HOME_DIR/profiles/$PROFILE/chat.db"

START_MS="$(perl -MTime::HiRes=time -e 'printf "%.0f\n", time() * 1000')"
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
  any(.event == "transcript") and
  any(.event == "dispatching" and (.action | contains("Aegis"))) and
  any(.event == "planning" and .tool == "Reobserving") and
  any(.event == "reply" and ((.text // "") | length > 0)) and
  (idx("transcript") < idx("dispatching")) and
  (idx("dispatching") < idx("reply")) and
  (map(select(.event == "reply")) | all((.text // "") | test("\\b[Cc]onfirmed\\b") | not)) and
  (map(select(.event == "reply")) | all((.text // "") | test("127\\.0\\.0\\.1|cua-[A-Za-z0-9_-]+") | not)) and
  (
    (map(select(.event == "reply" and ((.text // "") | contains("Planner provider stopped the task")))) | length == 0)
    or
    any(.event == "reply" and ((.text // "") | contains("completed attempt")) and ((.text // "") | contains("last progress was")))
  ) and
  any(.event == "metric" and .name == "dispatch_ms") and
  any(.event == "metric" and .name == "reobserve_ms") and
  any(.event == "metric" and .name == "turn_total_ms")
' "$EVENTS" >/dev/null

jq -s -e '
  def provider_stopped:
    any(.event == "reply" and ((.data.text // "") | contains("Planner provider stopped the task")));
  any(.event == "agent_loop_start" and .data.budget.kind == "unbounded") and
  any(.event == "dispatch_result" and .data.result.effect == "confirmed") and
  any(.event == "agent_attempt_outcome" and .data.long_range_continuation == true) and
  any(.event == "agent_reobserve_result") and
  any(.event == "agent_loop_stop") and
  any(.event == "memory_persisted") and
  (map(select(.event == "reply")) | all((.data.text // "") | test("\\b[Cc]onfirmed\\b") | not)) and
  (map(select(.event == "reply")) | all((.data.text // "") | test("127\\.0\\.0\\.1|cua-[A-Za-z0-9_-]+") | not)) and
  (
    (provider_stopped | not)
    or
    (
      any(.event == "reply" and ((.data.text // "") | contains("completed attempt")) and ((.data.text // "") | contains("last progress was"))) and
      any(.event == "planning_error" and ((.data.error // "") | contains("402 Payment Required"))) and
      any(.event == "agent_loop_stop" and .data.final_effect == "failed")
    )
  )
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
    schema_version: "cua.voice_provider_progress_proof.v1",
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
    dispatches: ($events | map(select(.event == "dispatching") | .action)),
    reply: (($events | map(select(.event == "reply")) | last).text),
    trace_outcomes: ($trace | map(select(.event == "agent_attempt_outcome") | .data)),
    memory_persisted: ($trace | any(.event == "memory_persisted"))
  }' > "$PROOF"

jq -e '.ok == true and .within_budget == true' "$PROOF" >/dev/null

echo "$OUT_DIR"
