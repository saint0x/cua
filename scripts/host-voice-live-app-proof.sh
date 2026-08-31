#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v jq >/dev/null
command -v osascript >/dev/null
command -v perl >/dev/null
command -v sqlite3 >/dev/null
command -v xcrun >/dev/null

export SDKROOT="${SDKROOT:-$(xcrun --sdk macosx --show-sdk-path)}"
export BINDGEN_EXTRA_CLANG_ARGS="${BINDGEN_EXTRA_CLANG_ARGS:--isysroot $SDKROOT}"

RUN_ID="$(date +%s)"
SUFFIX="${RUN_ID: -5}"
PROFILE="${CUA_VOICE_LIVE_APP_PROFILE:-qapp$SUFFIX}"
OUT_DIR="${CUA_VOICE_LIVE_APP_OUT_DIR:-artifacts/cua/voice-live-app-$RUN_ID}"
case "$OUT_DIR" in
  /*) ;;
  *) OUT_DIR="$ROOT/$OUT_DIR" ;;
esac
CUA_HOME_DIR="${CUA_VOICE_LIVE_APP_HOME:-}"
ENV_FILE="${CUA_ENV_FILE:-$HOME/.cua/config/env}"
PLANNER_MODEL="${CUA_VOICE_LIVE_APP_MODEL:-anthropic/claude-sonnet-4.6}"
BUDGET_MS="${CUA_VOICE_LIVE_APP_BUDGET_MS:-120000}"
TRACE="$OUT_DIR/voice.jsonl"
EVENTS="$OUT_DIR/events.jsonl"
PROOF="$OUT_DIR/proof.json"
TOKEN="APP-PROOF-$RUN_ID"
EXPECTED="ANSWER[app-textedit]=$TOKEN"
TRANSCRIPT="Use real Mac app actions, not shell_exec and not Aegis. Open TextEdit, create a new document, paste exactly $TOKEN, select all, copy it, read the clipboard to verify the pasted text, and then final reply must include exactly $EXPECTED."

env_key_available() {
  local name="$1"
  [[ -n "${!name:-}" ]] && return 0
  grep -Eq "^[[:space:]]*(export[[:space:]]+)?${name}=" "$ENV_FILE" 2>/dev/null && return 0
  return 1
}

if ! env_key_available OPENROUTER_API_KEY; then
  echo "OPENROUTER_API_KEY is required in the environment or $ENV_FILE" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"
if [[ -z "$CUA_HOME_DIR" ]]; then
  CUA_HOME_DIR="$(mktemp -d /tmp/cuaapp.XXXXXX)"
else
  mkdir -p "$CUA_HOME_DIR"
fi

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

cleanup() {
  osascript >/dev/null 2>&1 <<'APPLESCRIPT' || true
tell application "TextEdit"
  close every document saving no
  quit
end tell
APPLESCRIPT
}
trap cleanup EXIT

START_MS="$(perl -MTime::HiRes=time -e 'printf "%.0f\n", time() * 1000')"
CUA_HOME="$CUA_HOME_DIR" \
CUA_ENV_FILE="$ENV_FILE" \
CUA_VOICE_DEBUG_TRACE=true \
CUA_VOICE_TRACE_PATH="$TRACE" \
CUA_AGENT_LOOP_MAX_ATTEMPTS="n" \
"$VOICE_BIN_PATH" \
  --profile "$PROFILE" \
  --headless \
  --planner-model "$PLANNER_MODEL" \
  --once-agent-reply-wait-ms "$BUDGET_MS" \
  --once-transcript "$TRANSCRIPT" > "$EVENTS"
END_MS="$(perl -MTime::HiRes=time -e 'printf "%.0f\n", time() * 1000')"
ELAPSED_MS="$((END_MS - START_MS))"

CHAT_DB="$CUA_HOME_DIR/profiles/$PROFILE/chat.db"
if [[ ! -f "$TRACE" ]]; then
  echo "voice trace not found at $TRACE" >&2
  exit 1
fi
if [[ ! -f "$CHAT_DB" ]]; then
  echo "chat database not found at $CHAT_DB" >&2
  exit 1
fi

CHAT_ROWS="$(sqlite3 "$CHAT_DB" "select count(*) from chat_messages where role in ('user', 'assistant');")"
if [[ "$CHAT_ROWS" -lt 2 ]]; then
  echo "expected persisted user and assistant chat rows, got $CHAT_ROWS" >&2
  exit 1
fi

jq -s -e '
  any(.event == "agent_loop_start" and .data.budget.kind == "unbounded") and
  any(.event == "dispatch_result") and
  any(.event == "agent_attempt_outcome" and .data.has_action == true and .data.should_replan == true) and
  any(.event == "agent_loop_stop" and .data.final_effect == "confirmed") and
  any(.event == "reply" and ((.data.text // "") | contains($expected))) and
  any(.event == "memory_persisted") and
  (
    map(select(.event == "planning_result") | .data.action)
    | tostring
    | contains("\"open_app\"") and
      contains("\"TextEdit\"") and
      contains("\"key_paste\"") and
      contains($token) and
      contains("\"clipboard_read\"")
  ) and
  (
    map(select(.event == "dispatch_result") | .data.result.evidence)
    | tostring
    | contains($token)
  )
' --arg expected "$EXPECTED" --arg token "$TOKEN" "$TRACE" >/dev/null

jq -n \
  --arg profile "$PROFILE" \
  --arg planner_model "$PLANNER_MODEL" \
  --arg transcript "$TRANSCRIPT" \
  --arg expected "$EXPECTED" \
  --arg trace_path "$TRACE" \
  --arg events_path "$EVENTS" \
  --arg chat_db "$CHAT_DB" \
  --argjson elapsed_ms "$ELAPSED_MS" \
  --argjson budget_ms "$BUDGET_MS" \
  --slurpfile trace "$TRACE" \
  '{
    schema_version: "cua.voice_live_app_proof.v1",
    ok: true,
    profile: $profile,
    planner_model: $planner_model,
    transcript: $transcript,
    expected: $expected,
    elapsed_ms: $elapsed_ms,
    budget_ms: $budget_ms,
    within_budget: ($elapsed_ms <= $budget_ms),
    trace_path: $trace_path,
    events_path: $events_path,
    chat_db: $chat_db,
    events: ($trace | map(.event)),
    planned_actions: ($trace | map(select(.event == "planning_result") | .data.action)),
    trace_stop: (($trace | map(select(.event == "agent_loop_stop")) | last).data),
    reply: (($trace | map(select(.event == "reply")) | last).data.text),
    memory_persisted: ($trace | any(.event == "memory_persisted"))
  }' > "$PROOF"

jq -e '.ok == true and .within_budget == true' "$PROOF" >/dev/null
echo "$OUT_DIR"
