#!/usr/bin/env bash
set -euo pipefail

export CUA_DEV_HTTP_TOKEN_OVERRIDE=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v jq >/dev/null

RUN_ID="$(date +%s)"
OUT_DIR="${CUA_VOICE_PROOF_SUITE_OUT_DIR:-artifacts/cua/voice-proof-suite-$RUN_ID}"
MANIFEST="$OUT_DIR/proof.json"
PORT_BASE="$((19000 + (RUN_ID % 1000) * 2))"
mkdir -p "$OUT_DIR"

ACTION_DIR="$OUT_DIR/action"
PLANNER_DIR="$OUT_DIR/planner"
MISSING_KEY_DIR="$OUT_DIR/missing-key"
PROVIDER_PROGRESS_DIR="$OUT_DIR/provider-progress"
UI_DIR="$OUT_DIR/ui"

env_key_available() {
  local name="$1"
  [[ -n "${!name:-}" ]] && return 0
  grep -Eq "^[[:space:]]*(export[[:space:]]+)?${name}=" "${CUA_ENV_FILE:-$HOME/.cua/config/env}" 2>/dev/null && return 0
  return 1
}

ACTION_RESULT="$(
  CUA_HTTP_TOKEN="voice-proof-suite-action-$RUN_ID" \
  CUA_VOICE_WAV_PROOF_PROFILE="voice-proof-suite-action-$RUN_ID" \
  CUA_VOICE_WAV_PROOF_ADDR="127.0.0.1:$PORT_BASE" \
  CUA_VOICE_WAV_PROOF_OUT_DIR="$ACTION_DIR" \
  scripts/host-voice-wav-proof.sh | tail -n 1
)"
PLANNER_RESULT="$(
  CUA_HTTP_TOKEN="voice-proof-suite-planner-$RUN_ID" \
  CUA_VOICE_PLANNER_PROOF_PROFILE="voice-proof-suite-planner-$RUN_ID" \
  CUA_VOICE_PLANNER_PROOF_ADDR="127.0.0.1:$((PORT_BASE + 1))" \
  CUA_VOICE_PLANNER_PROOF_OUT_DIR="$PLANNER_DIR" \
  scripts/host-voice-planner-proof.sh | tail -n 1
)"
MISSING_KEY_RESULT="$(
  CUA_VOICE_MISSING_KEY_PROFILE="voice-proof-suite-missing-key-$RUN_ID" \
  CUA_VOICE_MISSING_KEY_OUT_DIR="$MISSING_KEY_DIR" \
  scripts/host-voice-missing-planner-key-proof.sh | tail -n 1
)"
PROVIDER_PROGRESS_RESULT=""
if env_key_available OPENROUTER_API_KEY; then
  PROVIDER_PROGRESS_RESULT="$(
    CUA_VOICE_PROVIDER_PROGRESS_PROFILE="voice-proof-suite-provider-$RUN_ID" \
    CUA_VOICE_PROVIDER_PROGRESS_OUT_DIR="$PROVIDER_PROGRESS_DIR" \
    scripts/host-voice-provider-progress-proof.sh | tail -n 1
  )"
fi
UI_RESULT="$(
  CUA_VOICE_UI_PROOF_PROFILE="voice-proof-suite-ui-$RUN_ID" \
  CUA_VOICE_UI_PROOF_OUT_DIR="$UI_DIR" \
  scripts/host-voice-ui-proof.sh | tail -n 1
)"

if [[ "$ACTION_RESULT" != "$ACTION_DIR" || "$PLANNER_RESULT" != "$PLANNER_DIR" || "$UI_RESULT" != "$UI_DIR" ]]; then
  echo "voice proof child output mismatch" >&2
  exit 1
fi
if [[ "$MISSING_KEY_RESULT" != "$MISSING_KEY_DIR" ]]; then
  echo "voice missing-key proof child output mismatch" >&2
  exit 1
fi
if [[ -n "$PROVIDER_PROGRESS_RESULT" && "$PROVIDER_PROGRESS_RESULT" != "$PROVIDER_PROGRESS_DIR" ]]; then
  echo "voice provider progress proof child output mismatch" >&2
  exit 1
fi

jq -e '.within_budget == true' "$ACTION_DIR/proof.json" >/dev/null
jq -e '.within_budget == true' "$PLANNER_DIR/proof.json" >/dev/null
jq -e '.ok == true and .within_budget == true' "$MISSING_KEY_DIR/proof.json" >/dev/null
if [[ -n "$PROVIDER_PROGRESS_RESULT" ]]; then
  jq -e '.ok == true and .within_budget == true' "$PROVIDER_PROGRESS_DIR/proof.json" >/dev/null
fi
jq -e '.ok == true' "$UI_DIR/proof.json" >/dev/null

if [[ -n "$PROVIDER_PROGRESS_RESULT" ]]; then
  PROVIDER_PROGRESS_ARG=(--slurpfile provider_progress "$PROVIDER_PROGRESS_DIR/proof.json")
else
  PROVIDER_PROGRESS_ARG=(--argjson provider_progress '[]')
fi

jq -n \
  --arg action_dir "$ACTION_DIR" \
  --arg planner_dir "$PLANNER_DIR" \
  --arg missing_key_dir "$MISSING_KEY_DIR" \
  --arg provider_progress_dir "$PROVIDER_PROGRESS_DIR" \
  --arg ui_dir "$UI_DIR" \
  --arg action_addr "127.0.0.1:$PORT_BASE" \
  --arg planner_addr "127.0.0.1:$((PORT_BASE + 1))" \
  --slurpfile action "$ACTION_DIR/proof.json" \
  --slurpfile planner "$PLANNER_DIR/proof.json" \
  --slurpfile missing_key "$MISSING_KEY_DIR/proof.json" \
  "${PROVIDER_PROGRESS_ARG[@]}" \
  --slurpfile ui "$UI_DIR/proof.json" \
  '{
    schema_version: "cua.voice_proof_suite.v1",
    ok: (
      $action[0].within_budget == true and
      $planner[0].within_budget == true and
      $ui[0].ok == true
    ),
    ports: {
      action: $action_addr,
      planner: $planner_addr
    },
    action: {
      dir: $action_dir,
      elapsed_ms: $action[0].elapsed_ms,
      events: $action[0].events,
      daemon_voice_steps: $action[0].daemon_voice_steps,
      transcript: $action[0].transcript,
      dispatch: $action[0].dispatch,
      reply: $action[0].reply,
      metrics: $action[0].metrics,
      safety_state: $action[0].safety_state
    },
    planner: {
      dir: $planner_dir,
      elapsed_ms: $planner[0].elapsed_ms,
      events: $planner[0].events,
      daemon_voice_steps: $planner[0].daemon_voice_steps,
      transcript: $planner[0].transcript,
      reply: $planner[0].reply,
      metrics: $planner[0].metrics,
      safety_state: $planner[0].safety_state
    },
    missing_key: {
      dir: $missing_key_dir,
      elapsed_ms: $missing_key[0].elapsed_ms,
      events: $missing_key[0].events,
      reply: $missing_key[0].reply,
      trace_stop: $missing_key[0].trace_stop,
      memory_persisted: $missing_key[0].memory_persisted
    },
    provider_progress: (
      if ($provider_progress | length) == 0 then
        {
          skipped: true,
          reason: "OPENROUTER_API_KEY unavailable",
          dir: $provider_progress_dir
        }
      else
        {
          skipped: false,
          dir: $provider_progress_dir,
          elapsed_ms: $provider_progress[0].elapsed_ms,
          events: $provider_progress[0].events,
          dispatches: $provider_progress[0].dispatches,
          reply: $provider_progress[0].reply,
          trace_outcomes: $provider_progress[0].trace_outcomes,
          memory_persisted: $provider_progress[0].memory_persisted
        }
      end
    ),
    ui: {
      dir: $ui_dir,
      screen: $ui[0].screen,
      compact_ok: $ui[0].compact.ok,
      reply_ok: $ui[0].reply.ok,
      collapsed_ok: $ui[0].collapsed.ok,
      island: {
        compact: $ui[0].compact.island,
        reply: $ui[0].reply.island,
        collapsed: $ui[0].collapsed.island
      }
    }
  }' > "$MANIFEST"

jq -e '.ok == true' "$MANIFEST" >/dev/null

echo "$OUT_DIR"
