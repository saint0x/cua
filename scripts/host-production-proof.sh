#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v jq >/dev/null

RUN_ID="$(date +%s)"
OUT_DIR="${CUA_PRODUCTION_PROOF_OUT_DIR:-artifacts/cua/production-proof-$RUN_ID}"
MANIFEST="$OUT_DIR/proof.json"
mkdir -p "$OUT_DIR"

VOICE_DIR="$OUT_DIR/voice"
CONTROL_DIR="$OUT_DIR/control"

VOICE_RESULT="$(
  CUA_VOICE_PROOF_SUITE_OUT_DIR="$VOICE_DIR" \
  scripts/host-voice-proof-suite.sh | tail -n 1
)"
CONTROL_RESULT="$(
  CUA_CONTROL_SURFACE_PROOF_OUT_DIR="$CONTROL_DIR" \
  scripts/host-control-surface-proof.sh | tail -n 1
)"

if [[ "$VOICE_RESULT" != "$VOICE_DIR" || "$CONTROL_RESULT" != "$CONTROL_DIR" ]]; then
  echo "production proof child output mismatch" >&2
  exit 1
fi

jq -e '.ok == true' "$VOICE_DIR/proof.json" >/dev/null
jq -e '.ok == true' "$CONTROL_DIR/proof.json" >/dev/null

jq -n \
  --arg voice_dir "$VOICE_DIR" \
  --arg control_dir "$CONTROL_DIR" \
  --slurpfile voice "$VOICE_DIR/proof.json" \
  --slurpfile control "$CONTROL_DIR/proof.json" \
  '{
    schema_version: "cua.production_proof.v1",
    ok: ($voice[0].ok == true and $control[0].ok == true),
    voice: {
      dir: $voice_dir,
      action_elapsed_ms: $voice[0].action.elapsed_ms,
      planner_elapsed_ms: $voice[0].planner.elapsed_ms,
      compact_ok: $voice[0].ui.compact_ok,
      reply_ok: $voice[0].ui.reply_ok,
      collapsed_ok: $voice[0].ui.collapsed_ok
    },
    control_surfaces: {
      dir: $control_dir,
      http: $control[0].http,
      cli: $control[0].cli,
      unix: $control[0].unix
    }
  }' > "$MANIFEST"

jq -e '.ok == true' "$MANIFEST" >/dev/null

echo "$OUT_DIR"
