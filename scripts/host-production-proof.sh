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
PACKAGE_DIR="$OUT_DIR/package"

VOICE_RESULT="$(
  CUA_VOICE_PROOF_SUITE_OUT_DIR="$VOICE_DIR" \
  scripts/host-voice-proof-suite.sh | tail -n 1
)"
CONTROL_RESULT="$(
  CUA_CONTROL_SURFACE_PROOF_OUT_DIR="$CONTROL_DIR" \
  scripts/host-control-surface-proof.sh | tail -n 1
)"
PACKAGE_RESULT="$(
  CUA_PACKAGE_PROOF_OUT_DIR="$PACKAGE_DIR" \
  scripts/host-package-proof.sh | tail -n 1
)"

if [[ "$VOICE_RESULT" != "$VOICE_DIR" || "$CONTROL_RESULT" != "$CONTROL_DIR" || "$PACKAGE_RESULT" != "$PACKAGE_DIR" ]]; then
  echo "production proof child output mismatch" >&2
  exit 1
fi

jq -e '.ok == true' "$VOICE_DIR/proof.json" >/dev/null
jq -e '.ok == true' "$CONTROL_DIR/proof.json" >/dev/null
jq -e '.ok == true' "$PACKAGE_DIR/proof.json" >/dev/null

jq -n \
  --arg voice_dir "$VOICE_DIR" \
  --arg control_dir "$CONTROL_DIR" \
  --arg package_dir "$PACKAGE_DIR" \
  --slurpfile voice "$VOICE_DIR/proof.json" \
  --slurpfile control "$CONTROL_DIR/proof.json" \
  --slurpfile package "$PACKAGE_DIR/proof.json" \
  '{
    schema_version: "cua.production_proof.v1",
    ok: ($voice[0].ok == true and $control[0].ok == true and $package[0].ok == true),
    voice: {
      dir: $voice_dir,
      action_elapsed_ms: $voice[0].action.elapsed_ms,
      planner_elapsed_ms: $voice[0].planner.elapsed_ms,
      action_events: $voice[0].action.events,
      planner_events: $voice[0].planner.events,
      action_daemon_steps: $voice[0].action.daemon_voice_steps,
      planner_daemon_steps: $voice[0].planner.daemon_voice_steps,
      action_metrics: $voice[0].action.metrics,
      planner_metrics: $voice[0].planner.metrics,
      missing_key: $voice[0].missing_key,
      provider_progress: $voice[0].provider_progress,
      compact_ok: $voice[0].ui.compact_ok,
      reply_ok: $voice[0].ui.reply_ok,
      collapsed_ok: $voice[0].ui.collapsed_ok,
      island: $voice[0].ui.island
    },
    control_surfaces: {
      dir: $control_dir,
      http: $control[0].http,
      cli: $control[0].cli,
      unix: $control[0].unix,
      programmable_replies: {
        http: $control[0].http.ui_reply,
        cli: $control[0].cli.ui_reply,
        unix: $control[0].unix.ui_reply,
        voice_bridge: $control[0].unix.voice_bridge.reply_text
      }
    },
    package: {
      dir: $package_dir,
      app_path: $package[0].app_path,
      bundle_id: $package[0].bundle_id,
      executable: $package[0].executable,
      lsui_element: $package[0].lsui_element,
      usage_descriptions: $package[0].usage_descriptions,
      binaries: $package[0].binaries
    }
  }' > "$MANIFEST"

jq -e '.ok == true' "$MANIFEST" >/dev/null

echo "$OUT_DIR"
