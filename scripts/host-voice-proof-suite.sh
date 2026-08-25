#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v jq >/dev/null

RUN_ID="$(date +%s)"
OUT_DIR="${CUA_VOICE_PROOF_SUITE_OUT_DIR:-artifacts/cua/voice-proof-suite-$RUN_ID}"
MANIFEST="$OUT_DIR/proof.json"
mkdir -p "$OUT_DIR"

ACTION_DIR="$(scripts/host-voice-wav-proof.sh | tail -n 1)"
PLANNER_DIR="$(scripts/host-voice-planner-proof.sh | tail -n 1)"
UI_DIR="$(scripts/host-voice-ui-proof.sh | tail -n 1)"

jq -e '.within_budget == true' "$ACTION_DIR/proof.json" >/dev/null
jq -e '.within_budget == true' "$PLANNER_DIR/proof.json" >/dev/null
jq -e '.ok == true' "$UI_DIR/proof.json" >/dev/null

jq -n \
  --arg action_dir "$ACTION_DIR" \
  --arg planner_dir "$PLANNER_DIR" \
  --arg ui_dir "$UI_DIR" \
  --slurpfile action "$ACTION_DIR/proof.json" \
  --slurpfile planner "$PLANNER_DIR/proof.json" \
  --slurpfile ui "$UI_DIR/proof.json" \
  '{
    schema_version: "cua.voice_proof_suite.v1",
    ok: (
      $action[0].within_budget == true and
      $planner[0].within_budget == true and
      $ui[0].ok == true
    ),
    action: {
      dir: $action_dir,
      elapsed_ms: $action[0].elapsed_ms,
      transcript: $action[0].transcript,
      dispatch: $action[0].dispatch,
      reply: $action[0].reply,
      metrics: $action[0].metrics,
      safety_state: $action[0].safety_state
    },
    planner: {
      dir: $planner_dir,
      elapsed_ms: $planner[0].elapsed_ms,
      transcript: $planner[0].transcript,
      reply: $planner[0].reply,
      metrics: $planner[0].metrics,
      safety_state: $planner[0].safety_state
    },
    ui: {
      dir: $ui_dir,
      screen: $ui[0].screen,
      compact_ok: $ui[0].compact.ok,
      reply_ok: $ui[0].reply.ok,
      collapsed_ok: $ui[0].collapsed.ok
    }
  }' > "$MANIFEST"

jq -e '.ok == true' "$MANIFEST" >/dev/null

echo "$OUT_DIR"
