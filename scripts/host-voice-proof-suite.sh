#!/usr/bin/env bash
set -euo pipefail

export CUA_DEV_HTTP_TOKEN_OVERRIDE=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v jq >/dev/null
command -v xcrun >/dev/null

export SDKROOT="${SDKROOT:-$(xcrun --sdk macosx --show-sdk-path)}"
export BINDGEN_EXTRA_CLANG_ARGS="${BINDGEN_EXTRA_CLANG_ARGS:--isysroot $SDKROOT}"

RUN_ID="$(date +%s)"
OUT_DIR="${CUA_VOICE_PROOF_SUITE_OUT_DIR:-artifacts/cua/voice-proof-suite-$RUN_ID}"
case "$OUT_DIR" in
  /*) ;;
  *) OUT_DIR="$ROOT/$OUT_DIR" ;;
esac
MANIFEST="$OUT_DIR/proof.json"
PORT_BASE="$((19000 + (RUN_ID % 1000) * 2))"
mkdir -p "$OUT_DIR"

ACTION_DIR="$OUT_DIR/action"
PLANNER_DIR="$OUT_DIR/planner"
LIVE_LONG_RANGE_DIR="$OUT_DIR/live-long-range-qualitative"
MISSING_KEY_DIR="$OUT_DIR/missing-key"
PROVIDER_PROGRESS_DIR="$OUT_DIR/provider-progress"
LIVE_APP_DIR="$OUT_DIR/live-app"
UI_DIR="$OUT_DIR/ui"
PLANNER_MODEL="${CUA_VOICE_PLANNER_PROOF_MODEL:-anthropic/claude-sonnet-4.6}"
INCLUDE_UI="${CUA_VOICE_PROOF_SUITE_INCLUDE_UI:-0}"

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
  CUA_VOICE_PLANNER_PROOF_MODEL="$PLANNER_MODEL" \
  scripts/host-voice-planner-proof.sh | tail -n 1
)"
LIVE_LONG_RANGE_RESULT="$(
  CUA_HTTP_TOKEN="voice-proof-suite-live-long-range-$RUN_ID" \
  CUA_VOICE_LIVE_LONG_RANGE_PROFILE="voice-proof-suite-live-long-range-$RUN_ID" \
  CUA_VOICE_LIVE_LONG_RANGE_OUT_DIR="$LIVE_LONG_RANGE_DIR" \
  CUA_VOICE_LIVE_LONG_RANGE_MODEL="$PLANNER_MODEL" \
  scripts/host-voice-live-long-range-proof.sh | tail -n 1
)"
MISSING_KEY_RESULT="$(
  CUA_VOICE_MISSING_KEY_PROFILE="voice-proof-suite-missing-key-$RUN_ID" \
  CUA_VOICE_MISSING_KEY_OUT_DIR="$MISSING_KEY_DIR" \
  scripts/host-voice-missing-planner-key-proof.sh | tail -n 1
)"
PROVIDER_PROGRESS_RESULT="$(
  CUA_VOICE_PROVIDER_PROGRESS_PROFILE="voice-proof-suite-provider-$RUN_ID" \
  CUA_VOICE_PROVIDER_PROGRESS_OUT_DIR="$PROVIDER_PROGRESS_DIR" \
  CUA_VOICE_PROVIDER_PROGRESS_MODEL="$PLANNER_MODEL" \
  scripts/host-voice-provider-progress-proof.sh | tail -n 1
)"
UI_RESULT=""
LIVE_APP_RESULT=""
if [[ "$INCLUDE_UI" == "1" ]]; then
  LIVE_APP_RESULT="$(
    CUA_VOICE_LIVE_APP_PROFILE="voice-proof-suite-app-$RUN_ID" \
    CUA_VOICE_LIVE_APP_OUT_DIR="$LIVE_APP_DIR" \
    CUA_VOICE_LIVE_APP_MODEL="$PLANNER_MODEL" \
    scripts/host-voice-live-app-proof.sh | tail -n 1
  )"
  UI_RESULT="$(
    CUA_VOICE_UI_PROOF_PROFILE="voice-proof-suite-ui-$RUN_ID" \
    CUA_VOICE_UI_PROOF_OUT_DIR="$UI_DIR" \
    scripts/host-voice-ui-proof.sh | tail -n 1
  )"
fi

if [[ "$ACTION_RESULT" != "$ACTION_DIR" ]]; then
  echo "voice proof child output mismatch" >&2
  exit 1
fi
if [[ "$INCLUDE_UI" == "1" && "$UI_RESULT" != "$UI_DIR" ]]; then
  echo "voice UI proof child output mismatch" >&2
  exit 1
fi
if [[ "$INCLUDE_UI" == "1" && "$LIVE_APP_RESULT" != "$LIVE_APP_DIR" ]]; then
  echo "voice live app proof child output mismatch" >&2
  exit 1
fi
if [[ "$PLANNER_RESULT" != "$PLANNER_DIR" ]]; then
  echo "voice planner proof child output mismatch" >&2
  exit 1
fi
if [[ "$LIVE_LONG_RANGE_RESULT" != "$LIVE_LONG_RANGE_DIR" ]]; then
  echo "voice live long-range qualitative proof child output mismatch" >&2
  exit 1
fi
if [[ "$MISSING_KEY_RESULT" != "$MISSING_KEY_DIR" ]]; then
  echo "voice missing-key proof child output mismatch" >&2
  exit 1
fi
if [[ "$PROVIDER_PROGRESS_RESULT" != "$PROVIDER_PROGRESS_DIR" ]]; then
  echo "voice provider progress proof child output mismatch" >&2
  exit 1
fi
jq -e '.within_budget == true' "$ACTION_DIR/proof.json" >/dev/null
jq -e '.within_budget == true' "$PLANNER_DIR/proof.json" >/dev/null
jq -e '
  .ok == true and
  .scenario_count >= 10 and
  .ok_count == .scenario_count and
  (.results | length) == .scenario_count and
  all(.results[]; .ok == true and .planner_requests >= 2 and .mini_turn_requests >= 1 and .final_effect != null)
' "$LIVE_LONG_RANGE_DIR/proof.json" >/dev/null
jq -e '
  .ok == true and
  .within_budget == true and
  .trace_stop.attempts > 0 and
  .trace_stop.final_effect == "failed" and
  .memory_persisted == true
' "$MISSING_KEY_DIR/proof.json" >/dev/null
jq -e '
  .ok == true and
  .within_budget == true and
  (.trace_stop.attempts | type == "number") and
  .trace_stop.attempts > 0 and
  (.trace_stop.final_effect | IN("confirmed", "failed", "partial")) and
  any(.trace_outcomes[]; .has_action == true and .should_replan == true) and
  .memory_persisted == true
' "$PROVIDER_PROGRESS_DIR/proof.json" >/dev/null
if [[ "$INCLUDE_UI" == "1" ]]; then
  jq -e '.ok == true and .within_budget == true' "$LIVE_APP_DIR/proof.json" >/dev/null
  jq -e '.ok == true' "$UI_DIR/proof.json" >/dev/null
fi

if [[ "$INCLUDE_UI" == "1" ]]; then
  LIVE_APP_ARG=(--slurpfile live_app "$LIVE_APP_DIR/proof.json")
  UI_ARG=(--slurpfile ui "$UI_DIR/proof.json")
else
  LIVE_APP_ARG=(--argjson live_app '[]')
  UI_ARG=(--argjson ui '[]')
fi

jq -n \
  --arg action_dir "$ACTION_DIR" \
  --arg planner_dir "$PLANNER_DIR" \
  --arg live_long_range_dir "$LIVE_LONG_RANGE_DIR" \
  --arg missing_key_dir "$MISSING_KEY_DIR" \
  --arg provider_progress_dir "$PROVIDER_PROGRESS_DIR" \
  --arg live_app_dir "$LIVE_APP_DIR" \
  --arg ui_dir "$UI_DIR" \
  --argjson include_ui "$INCLUDE_UI" \
  --arg action_addr "127.0.0.1:$PORT_BASE" \
  --arg planner_addr "127.0.0.1:$((PORT_BASE + 1))" \
  --slurpfile action "$ACTION_DIR/proof.json" \
  --slurpfile planner "$PLANNER_DIR/proof.json" \
  --slurpfile live_long_range "$LIVE_LONG_RANGE_DIR/proof.json" \
  --slurpfile missing_key "$MISSING_KEY_DIR/proof.json" \
  --slurpfile provider_progress "$PROVIDER_PROGRESS_DIR/proof.json" \
  "${LIVE_APP_ARG[@]}" \
  "${UI_ARG[@]}" \
  '{
    schema_version: "cua.voice_proof_suite.v1",
    ok: (
      $action[0].within_budget == true and
      $planner[0].within_budget == true and
      $live_long_range[0].ok == true and
      $live_long_range[0].scenario_count >= 10 and
      $live_long_range[0].ok_count == $live_long_range[0].scenario_count and
      $missing_key[0].ok == true and
      $missing_key[0].within_budget == true and
      $missing_key[0].trace_stop.attempts > 0 and
      $missing_key[0].trace_stop.final_effect == "failed" and
      $missing_key[0].memory_persisted == true and
      $provider_progress[0].ok == true and
      $provider_progress[0].within_budget == true and
      ($provider_progress[0].trace_stop.attempts | type == "number") and
      $provider_progress[0].trace_stop.attempts > 0 and
      ($provider_progress[0].trace_stop.final_effect | IN("confirmed", "failed", "partial")) and
      any($provider_progress[0].trace_outcomes[]; .has_action == true and .should_replan == true) and
      $provider_progress[0].memory_persisted == true and
      ((($live_app | length) == 0 and $include_ui == 0) or (($live_app | length) > 0 and $live_app[0].ok == true)) and
      ((($ui | length) == 0 and $include_ui == 0) or (($ui | length) > 0 and $ui[0].ok == true))
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
    live_long_range_qualitative: {
      dir: $live_long_range_dir,
      scenario_count: $live_long_range[0].scenario_count,
      ok_count: $live_long_range[0].ok_count,
      failed: $live_long_range[0].failed,
      results: $live_long_range[0].results
    },
    missing_key: {
      dir: $missing_key_dir,
      elapsed_ms: $missing_key[0].elapsed_ms,
      events: $missing_key[0].events,
      reply: $missing_key[0].reply,
      trace_stop: $missing_key[0].trace_stop,
      memory_persisted: $missing_key[0].memory_persisted
    },
    provider_progress: {
      dir: $provider_progress_dir,
      elapsed_ms: $provider_progress[0].elapsed_ms,
      events: $provider_progress[0].events,
      dispatches: $provider_progress[0].dispatches,
      reply: $provider_progress[0].reply,
      trace_stop: $provider_progress[0].trace_stop,
      trace_outcomes: $provider_progress[0].trace_outcomes,
      memory_persisted: $provider_progress[0].memory_persisted
    },
    live_app: (
      if ($live_app | length) == 0 then
        {
          skipped: true,
          reason: "headful app proof disabled; set CUA_VOICE_PROOF_SUITE_INCLUDE_UI=1",
          dir: $live_app_dir
        }
      else
        {
          skipped: false,
          dir: $live_app_dir,
          elapsed_ms: $live_app[0].elapsed_ms,
          planned_actions: $live_app[0].planned_actions,
          reply: $live_app[0].reply,
          trace_stop: $live_app[0].trace_stop,
          memory_persisted: $live_app[0].memory_persisted
        }
      end
    ),
    ui: (
      if ($ui | length) == 0 then
        {
          skipped: true,
          reason: "headful UI proof disabled; set CUA_VOICE_PROOF_SUITE_INCLUDE_UI=1",
          dir: $ui_dir
        }
      else
        {
          skipped: false,
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
      end
    )
  }' > "$MANIFEST"

jq -e '.ok == true' "$MANIFEST" >/dev/null

echo "$OUT_DIR"
