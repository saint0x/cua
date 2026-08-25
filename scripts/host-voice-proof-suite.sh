#!/usr/bin/env bash
set -euo pipefail

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
UI_DIR="$OUT_DIR/ui"

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
UI_RESULT="$(
  CUA_VOICE_UI_PROOF_PROFILE="voice-proof-suite-ui-$RUN_ID" \
  CUA_VOICE_UI_PROOF_OUT_DIR="$UI_DIR" \
  scripts/host-voice-ui-proof.sh | tail -n 1
)"

if [[ "$ACTION_RESULT" != "$ACTION_DIR" || "$PLANNER_RESULT" != "$PLANNER_DIR" || "$UI_RESULT" != "$UI_DIR" ]]; then
  echo "voice proof child output mismatch" >&2
  exit 1
fi

jq -e '.within_budget == true' "$ACTION_DIR/proof.json" >/dev/null
jq -e '.within_budget == true' "$PLANNER_DIR/proof.json" >/dev/null
jq -e '.ok == true' "$UI_DIR/proof.json" >/dev/null

jq -n \
  --arg action_dir "$ACTION_DIR" \
  --arg planner_dir "$PLANNER_DIR" \
  --arg ui_dir "$UI_DIR" \
  --arg action_addr "127.0.0.1:$PORT_BASE" \
  --arg planner_addr "127.0.0.1:$((PORT_BASE + 1))" \
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
    ports: {
      action: $action_addr,
      planner: $planner_addr
    },
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
