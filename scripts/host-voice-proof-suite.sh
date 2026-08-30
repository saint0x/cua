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
LOCAL_PLANNER_MISSING_DIR="$OUT_DIR/local-planner-missing-readback"
LOCAL_PLANNER_MISMATCH_DIR="$OUT_DIR/local-planner-mismatch-readback"
LOCAL_PLANNER_FAILED_DIR="$OUT_DIR/local-planner-failed-action-repair"
LOCAL_PLANNER_STALL_DIR="$OUT_DIR/local-planner-repeated-rejected-plan"
MISSING_KEY_DIR="$OUT_DIR/missing-key"
PROVIDER_PROGRESS_DIR="$OUT_DIR/provider-progress"
UI_DIR="$OUT_DIR/ui"
PLANNER_MODEL="${CUA_VOICE_PLANNER_PROOF_MODEL:-anthropic/claude-sonnet-4.6}"
INCLUDE_UI="${CUA_VOICE_PROOF_SUITE_INCLUDE_UI:-0}"

env_key_available() {
  local name="$1"
  [[ -n "${!name:-}" ]] && return 0
  grep -Eq "^[[:space:]]*(export[[:space:]]+)?${name}=" "${CUA_ENV_FILE:-$HOME/.cua/config/env}" 2>/dev/null && return 0
  return 1
}

planner_key_available() {
  env_key_available OPENROUTER_API_KEY
}

planner_key_name() {
  printf 'OPENROUTER_API_KEY'
}

ACTION_RESULT="$(
  CUA_HTTP_TOKEN="voice-proof-suite-action-$RUN_ID" \
  CUA_VOICE_WAV_PROOF_PROFILE="voice-proof-suite-action-$RUN_ID" \
  CUA_VOICE_WAV_PROOF_ADDR="127.0.0.1:$PORT_BASE" \
  CUA_VOICE_WAV_PROOF_OUT_DIR="$ACTION_DIR" \
  scripts/host-voice-wav-proof.sh | tail -n 1
)"
PLANNER_RESULT=""
if planner_key_available; then
  PLANNER_RESULT="$(
    CUA_HTTP_TOKEN="voice-proof-suite-planner-$RUN_ID" \
    CUA_VOICE_PLANNER_PROOF_PROFILE="voice-proof-suite-planner-$RUN_ID" \
    CUA_VOICE_PLANNER_PROOF_ADDR="127.0.0.1:$((PORT_BASE + 1))" \
    CUA_VOICE_PLANNER_PROOF_OUT_DIR="$PLANNER_DIR" \
    scripts/host-voice-planner-proof.sh | tail -n 1
  )"
fi
LOCAL_PLANNER_MISSING_RESULT="$(
  CUA_HTTP_TOKEN="voice-proof-suite-local-missing-$RUN_ID" \
  CUA_VOICE_LOCAL_PLANNER_PROFILE="voice-proof-suite-local-missing-$RUN_ID" \
  CUA_VOICE_LOCAL_PLANNER_ADDR="127.0.0.1:$((PORT_BASE + 2))" \
  CUA_VOICE_LOCAL_PLANNER_MODEL_ADDR="127.0.0.1:$((PORT_BASE + 3))" \
  CUA_VOICE_LOCAL_PLANNER_OUT_DIR="$LOCAL_PLANNER_MISSING_DIR" \
  CUA_VOICE_LOCAL_PLANNER_SCENARIO="missing-readback" \
  scripts/host-voice-local-planner-loop-proof.sh | tail -n 1
)"
LOCAL_PLANNER_MISMATCH_RESULT="$(
  CUA_HTTP_TOKEN="voice-proof-suite-local-mismatch-$RUN_ID" \
  CUA_VOICE_LOCAL_PLANNER_PROFILE="voice-proof-suite-local-mismatch-$RUN_ID" \
  CUA_VOICE_LOCAL_PLANNER_ADDR="127.0.0.1:$((PORT_BASE + 4))" \
  CUA_VOICE_LOCAL_PLANNER_MODEL_ADDR="127.0.0.1:$((PORT_BASE + 5))" \
  CUA_VOICE_LOCAL_PLANNER_OUT_DIR="$LOCAL_PLANNER_MISMATCH_DIR" \
  CUA_VOICE_LOCAL_PLANNER_SCENARIO="mismatch-readback" \
  scripts/host-voice-local-planner-loop-proof.sh | tail -n 1
)"
LOCAL_PLANNER_FAILED_RESULT="$(
  CUA_HTTP_TOKEN="voice-proof-suite-local-failed-$RUN_ID" \
  CUA_VOICE_LOCAL_PLANNER_PROFILE="voice-proof-suite-local-failed-$RUN_ID" \
  CUA_VOICE_LOCAL_PLANNER_ADDR="127.0.0.1:$((PORT_BASE + 6))" \
  CUA_VOICE_LOCAL_PLANNER_MODEL_ADDR="127.0.0.1:$((PORT_BASE + 7))" \
  CUA_VOICE_LOCAL_PLANNER_OUT_DIR="$LOCAL_PLANNER_FAILED_DIR" \
  CUA_VOICE_LOCAL_PLANNER_SCENARIO="failed-action-repair" \
  scripts/host-voice-local-planner-loop-proof.sh | tail -n 1
)"
LOCAL_PLANNER_STALL_RESULT="$(
  CUA_HTTP_TOKEN="voice-proof-suite-local-stall-$RUN_ID" \
  CUA_VOICE_LOCAL_PLANNER_PROFILE="voice-proof-suite-local-stall-$RUN_ID" \
  CUA_VOICE_LOCAL_PLANNER_ADDR="127.0.0.1:$((PORT_BASE + 8))" \
  CUA_VOICE_LOCAL_PLANNER_MODEL_ADDR="127.0.0.1:$((PORT_BASE + 9))" \
  CUA_VOICE_LOCAL_PLANNER_OUT_DIR="$LOCAL_PLANNER_STALL_DIR" \
  CUA_VOICE_LOCAL_PLANNER_SCENARIO="repeated-rejected-plan" \
  scripts/host-voice-local-planner-loop-proof.sh | tail -n 1
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
UI_RESULT=""
if [[ "$INCLUDE_UI" == "1" ]]; then
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
if [[ -n "$PLANNER_RESULT" && "$PLANNER_RESULT" != "$PLANNER_DIR" ]]; then
  echo "voice planner proof child output mismatch" >&2
  exit 1
fi
if [[ "$LOCAL_PLANNER_MISSING_RESULT" != "$LOCAL_PLANNER_MISSING_DIR" ]]; then
  echo "voice local planner missing-readback proof child output mismatch" >&2
  exit 1
fi
if [[ "$LOCAL_PLANNER_MISMATCH_RESULT" != "$LOCAL_PLANNER_MISMATCH_DIR" ]]; then
  echo "voice local planner mismatch-readback proof child output mismatch" >&2
  exit 1
fi
if [[ "$LOCAL_PLANNER_FAILED_RESULT" != "$LOCAL_PLANNER_FAILED_DIR" ]]; then
  echo "voice local planner failed-action-repair proof child output mismatch" >&2
  exit 1
fi
if [[ "$LOCAL_PLANNER_STALL_RESULT" != "$LOCAL_PLANNER_STALL_DIR" ]]; then
  echo "voice local planner repeated-rejected-plan proof child output mismatch" >&2
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
if [[ -n "$PLANNER_RESULT" ]]; then
  jq -e '.within_budget == true' "$PLANNER_DIR/proof.json" >/dev/null
fi
jq -e '
  .ok == true and
  .scenario == "missing-readback" and
  .final_reply == .expected and
  .planner_requests == 3 and
  .repair_context_preserved_partial_evidence == true and
  .model_visible_confirmed_leaked == false and
  .premature_final_rejected == true and
  .partial_repaired == true
' "$LOCAL_PLANNER_MISSING_DIR/proof.json" >/dev/null
jq -e '
  .ok == true and
  .scenario == "mismatch-readback" and
  .final_reply == .expected and
  .planner_requests == 3 and
  .repair_context_preserved_partial_evidence == true and
  .model_visible_confirmed_leaked == false and
  .premature_final_rejected == true and
  .partial_repaired == true
' "$LOCAL_PLANNER_MISMATCH_DIR/proof.json" >/dev/null
jq -e '
  .ok == true and
  .scenario == "failed-action-repair" and
  .final_reply == .expected and
  .planner_requests == 2 and
  .failed_evidence_reached_model == true and
  .failed_error_reached_model == true and
  .repaired_after_failure == true
' "$LOCAL_PLANNER_FAILED_DIR/proof.json" >/dev/null
jq -e '
  .ok == true and
  .scenario == "repeated-rejected-plan" and
  .planner_requests == 3 and
  .fake_success_suppressed == true and
  .dispatch_suppressed == true and
  .contextual_error_emitted == true and
  .planning_stalled == true
' "$LOCAL_PLANNER_STALL_DIR/proof.json" >/dev/null
jq -e '
  .ok == true and
  .within_budget == true and
  .trace_stop.attempts > 0 and
  .trace_stop.final_effect == "failed" and
  .memory_persisted == true
' "$MISSING_KEY_DIR/proof.json" >/dev/null
if [[ -n "$PROVIDER_PROGRESS_RESULT" ]]; then
  jq -e '
    .ok == true and
    .within_budget == true and
    (.trace_stop.attempts | type == "number") and
    .trace_stop.attempts > 0 and
    (.trace_stop.final_effect | IN("confirmed", "failed", "partial")) and
    any(.trace_outcomes[]; .has_action == true and .should_replan == true) and
    .memory_persisted == true
  ' "$PROVIDER_PROGRESS_DIR/proof.json" >/dev/null
fi
if [[ "$INCLUDE_UI" == "1" ]]; then
  jq -e '.ok == true' "$UI_DIR/proof.json" >/dev/null
fi

if [[ -n "$PROVIDER_PROGRESS_RESULT" ]]; then
  PROVIDER_PROGRESS_ARG=(--slurpfile provider_progress "$PROVIDER_PROGRESS_DIR/proof.json")
else
  PROVIDER_PROGRESS_ARG=(--argjson provider_progress '[]')
fi
if [[ -n "$PLANNER_RESULT" ]]; then
  PLANNER_ARG=(--slurpfile planner "$PLANNER_DIR/proof.json")
else
  PLANNER_ARG=(--argjson planner '[]')
fi
if [[ "$INCLUDE_UI" == "1" ]]; then
  UI_ARG=(--slurpfile ui "$UI_DIR/proof.json")
else
  UI_ARG=(--argjson ui '[]')
fi

jq -n \
  --arg action_dir "$ACTION_DIR" \
  --arg planner_dir "$PLANNER_DIR" \
  --arg planner_skip_reason "$(planner_key_name) unavailable" \
  --arg local_planner_missing_dir "$LOCAL_PLANNER_MISSING_DIR" \
  --arg local_planner_mismatch_dir "$LOCAL_PLANNER_MISMATCH_DIR" \
  --arg local_planner_failed_dir "$LOCAL_PLANNER_FAILED_DIR" \
  --arg local_planner_stall_dir "$LOCAL_PLANNER_STALL_DIR" \
  --arg missing_key_dir "$MISSING_KEY_DIR" \
  --arg provider_progress_dir "$PROVIDER_PROGRESS_DIR" \
  --arg ui_dir "$UI_DIR" \
  --argjson include_ui "$INCLUDE_UI" \
  --arg action_addr "127.0.0.1:$PORT_BASE" \
  --arg planner_addr "127.0.0.1:$((PORT_BASE + 1))" \
  --slurpfile action "$ACTION_DIR/proof.json" \
  "${PLANNER_ARG[@]}" \
  --slurpfile local_planner_missing "$LOCAL_PLANNER_MISSING_DIR/proof.json" \
  --slurpfile local_planner_mismatch "$LOCAL_PLANNER_MISMATCH_DIR/proof.json" \
  --slurpfile local_planner_failed "$LOCAL_PLANNER_FAILED_DIR/proof.json" \
  --slurpfile local_planner_stall "$LOCAL_PLANNER_STALL_DIR/proof.json" \
  --slurpfile missing_key "$MISSING_KEY_DIR/proof.json" \
  "${PROVIDER_PROGRESS_ARG[@]}" \
  "${UI_ARG[@]}" \
  '{
    schema_version: "cua.voice_proof_suite.v1",
    ok: (
      $action[0].within_budget == true and
      (($planner | length) == 0 or $planner[0].within_budget == true) and
      $local_planner_missing[0].ok == true and
      $local_planner_missing[0].scenario == "missing-readback" and
      $local_planner_missing[0].final_reply == $local_planner_missing[0].expected and
      $local_planner_missing[0].partial_repaired == true and
      $local_planner_mismatch[0].ok == true and
      $local_planner_mismatch[0].scenario == "mismatch-readback" and
      $local_planner_mismatch[0].final_reply == $local_planner_mismatch[0].expected and
      $local_planner_mismatch[0].partial_repaired == true and
      $local_planner_failed[0].ok == true and
      $local_planner_failed[0].scenario == "failed-action-repair" and
      $local_planner_failed[0].final_reply == $local_planner_failed[0].expected and
      $local_planner_failed[0].failed_evidence_reached_model == true and
      $local_planner_failed[0].failed_error_reached_model == true and
      $local_planner_failed[0].repaired_after_failure == true and
      $local_planner_stall[0].ok == true and
      $local_planner_stall[0].scenario == "repeated-rejected-plan" and
      $local_planner_stall[0].fake_success_suppressed == true and
      $local_planner_stall[0].contextual_error_emitted == true and
      $local_planner_stall[0].planning_stalled == true and
      $missing_key[0].ok == true and
      $missing_key[0].within_budget == true and
      $missing_key[0].trace_stop.attempts > 0 and
      $missing_key[0].trace_stop.final_effect == "failed" and
      $missing_key[0].memory_persisted == true and
      (
        ($provider_progress | length) == 0 or
        (
          $provider_progress[0].ok == true and
          $provider_progress[0].within_budget == true and
          ($provider_progress[0].trace_stop.attempts | type == "number") and
          $provider_progress[0].trace_stop.attempts > 0 and
          ($provider_progress[0].trace_stop.final_effect | IN("confirmed", "failed", "partial")) and
          any($provider_progress[0].trace_outcomes[]; .has_action == true and .should_replan == true) and
          $provider_progress[0].memory_persisted == true
        )
      ) and
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
    planner: (
      if ($planner | length) == 0 then
        {
          skipped: true,
          reason: $planner_skip_reason,
          dir: $planner_dir
        }
      else
        {
          skipped: false,
          dir: $planner_dir,
          elapsed_ms: $planner[0].elapsed_ms,
          events: $planner[0].events,
          daemon_voice_steps: $planner[0].daemon_voice_steps,
          transcript: $planner[0].transcript,
          reply: $planner[0].reply,
          metrics: $planner[0].metrics,
          safety_state: $planner[0].safety_state
        }
      end
    ),
    local_planner: {
      missing_readback: {
        dir: $local_planner_missing_dir,
        final_reply: $local_planner_missing[0].final_reply,
        planner_requests: $local_planner_missing[0].planner_requests,
        premature_final_rejected: $local_planner_missing[0].premature_final_rejected,
        partial_repaired: $local_planner_missing[0].partial_repaired
      },
      mismatch_readback: {
        dir: $local_planner_mismatch_dir,
        final_reply: $local_planner_mismatch[0].final_reply,
        planner_requests: $local_planner_mismatch[0].planner_requests,
        premature_final_rejected: $local_planner_mismatch[0].premature_final_rejected,
        partial_repaired: $local_planner_mismatch[0].partial_repaired
      },
      failed_action_repair: {
        dir: $local_planner_failed_dir,
        final_reply: $local_planner_failed[0].final_reply,
        planner_requests: $local_planner_failed[0].planner_requests,
        failed_evidence_reached_model: $local_planner_failed[0].failed_evidence_reached_model,
        failed_error_reached_model: $local_planner_failed[0].failed_error_reached_model,
        repaired_after_failure: $local_planner_failed[0].repaired_after_failure
      },
      repeated_rejected_plan: {
        dir: $local_planner_stall_dir,
        planner_requests: $local_planner_stall[0].planner_requests,
        fake_success_suppressed: $local_planner_stall[0].fake_success_suppressed,
        dispatch_suppressed: $local_planner_stall[0].dispatch_suppressed,
        contextual_error_emitted: $local_planner_stall[0].contextual_error_emitted,
        planning_stalled: $local_planner_stall[0].planning_stalled
      }
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
          trace_stop: $provider_progress[0].trace_stop,
          trace_outcomes: $provider_progress[0].trace_outcomes,
          memory_persisted: $provider_progress[0].memory_persisted
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
