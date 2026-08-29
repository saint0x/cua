#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="${CUA_APP_NAME:-cua}"
INSTALL_DIR="${CUA_APP_INSTALL_DIR:-$HOME/Applications}"
INSTALL_APP="$INSTALL_DIR/$APP_NAME.app"
RUN_ID="$(date +%Y%m%d-%H%M%S)"
ARTIFACT_DIR="${CUA_RELEASE_ARTIFACT_DIR:-$ROOT/artifacts/cua/release/$RUN_ID}"
SEED="${CUA_RELEASE_SEED:-4242}"
SCENARIO="${CUA_RELEASE_SCENARIO:-fozzy/scenarios/cua-smoke.json}"
RUNEBOOK_SCENARIO="${CUA_RELEASE_RUNEBOOK_SCENARIO:-fozzy/scenarios/cua-runebook.json}"
SDK_SCENARIO="${CUA_RELEASE_SDK_SCENARIO:-fozzy/scenarios/cua-sdk-action.json}"
SCRATCHPAD_SCENARIO="${CUA_RELEASE_SCRATCHPAD_SCENARIO:-fozzy/scenarios/cua-scratchpad.json}"
STT_BACKEND="${CUA_RELEASE_STT_BACKEND:-local}"
STT_MODEL="${CUA_RELEASE_STT_MODEL:-tiny.en}"
PLANNER_MODEL="${CUA_RELEASE_PLANNER_MODEL:-google/gemini-3.7-flash}"
SKIP_TESTS=0
SKIP_FOZZY=0
SKIP_INSTALL=0
SKIP_RELAUNCH=0
SKIP_LIVE=0

usage() {
  cat <<USAGE
usage: scripts/release.sh [options]

Build, test, package, install, relaunch, and verify the local macOS cua app.

Options:
  --skip-tests      Skip cargo fmt/check/test.
  --skip-fozzy      Skip deterministic Fozzy smoke and trace verification.
  --skip-install    Package only; do not copy to ~/Applications.
  --skip-relaunch   Do not restart the installed app after install.
  --skip-live       Skip installed voice WAV smoke.
  -h, --help        Show this help.

Environment:
  CUA_RELEASE_STT_BACKEND   Speech-to-text backend for live smoke. Default: local.
  CUA_RELEASE_STT_MODEL     Speech-to-text model for live smoke. Default: tiny.en.
  CUA_RELEASE_PLANNER_MODEL Planner model for live smoke. Default: google/gemini-3.7-flash.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-tests) SKIP_TESTS=1 ;;
    --skip-fozzy) SKIP_FOZZY=1 ;;
    --skip-install) SKIP_INSTALL=1 ;;
    --skip-relaunch) SKIP_RELAUNCH=1 ;;
    --skip-live) SKIP_LIVE=1 ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'unknown option: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

log() {
  printf '\n==> %s\n' "$*" >&2
}

run() {
  printf '+' >&2
  printf ' %q' "$@" >&2
  printf '\n' >&2
  "$@"
}

openrouter_key_available() {
  [[ -n "${OPENROUTER_API_KEY:-}" ]] && return 0
  grep -Eq '^[[:space:]]*(export[[:space:]]+)?OPENROUTER_API_KEY=' "${CUA_HOME:-$HOME/.cua}/config/env" 2>/dev/null && return 0
  return 1
}

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    printf 'required dependency missing: %s\n' "$1" >&2
    exit 1
  fi
}

require_file() {
  if [[ ! -e "$1" ]]; then
    printf 'required file missing: %s\n' "$1" >&2
    exit 1
  fi
}

require_no_generated_diff() {
  local changed
  changed="$(git diff --name-only -- \
    tests/fixtures/schema-bundle.json \
    sdks/typescript/dist/index.d.ts \
    sdks/typescript/dist/index.js)"
  if [[ -n "$changed" ]]; then
    printf 'generated artifacts have uncommitted changes:\n%s\n' "$changed" >&2
    exit 1
  fi
}

fozzy_trace_gate() {
  local scenario="$1"
  local name="$2"
  local trace="$ARTIFACT_DIR/$name.fozzy"

  require_file "$scenario"
  log "Fozzy deterministic $name"
  run fozzy doctor --deep --scenario "$scenario" --runs 5 --seed "$SEED" --json
  run fozzy test --det --strict-verify "$scenario" --json
  run fozzy run "$scenario" --det --record "$trace" --json
  run fozzy trace verify "$trace" --strict --json
  run fozzy replay "$trace" --json
  run fozzy ci "$trace" --json
}

generate_voice_fixture() {
  local out="$ARTIFACT_DIR/voice-smoke.wav"
  local aiff="$ARTIFACT_DIR/voice-smoke.aiff"
  mkdir -p "$ARTIFACT_DIR"
  run /usr/bin/say -o "$aiff" "what do you see on my screen"
  run /usr/bin/afconvert -f WAVE -d LEI16@16000 "$aiff" "$out"
  printf '%s\n' "$out"
}

restart_installed_app() {
  local pids
  pids="$(pgrep -f "^$INSTALL_APP/Contents/MacOS/cua" || true)"
  if [[ -n "$pids" ]]; then
    kill $(printf '%s\n' "$pids")
    sleep 1
  fi
  run /usr/bin/open "$INSTALL_APP"
  sleep 3
}

cd "$ROOT"
mkdir -p "$ARTIFACT_DIR"

require_cmd cargo
require_cmd git
require_cmd jq
require_cmd curl
require_cmd plutil
require_cmd /usr/bin/codesign
if [[ "$SKIP_FOZZY" -eq 0 ]]; then
  require_cmd fozzy
fi

log "Release config"
printf 'artifact_dir=%s\n' "$ARTIFACT_DIR"
printf 'smoke_scenario=%s\n' "$SCENARIO"
printf 'runebook_scenario=%s\n' "$RUNEBOOK_SCENARIO"
printf 'sdk_scenario=%s\n' "$SDK_SCENARIO"
printf 'scratchpad_scenario=%s\n' "$SCRATCHPAD_SCENARIO"
printf 'stt_backend=%s\n' "$STT_BACKEND"
printf 'stt_model=%s\n' "$STT_MODEL"
printf 'planner_model=%s\n' "$PLANNER_MODEL"

if [[ "$SKIP_TESTS" -eq 0 ]]; then
  log "Format and compile checks"
  run cargo fmt --check
  run git diff --check
  run cargo check
  run cargo check -p cua-voice --all-targets
  run cargo build -p cua

  log "Unit tests"
  run cargo test
  run cargo test -p cua-core --test schema_compat
  run cargo test -p cua-voice --lib
  run cargo test -p cua-voice --bin cua-voice
  require_no_generated_diff
fi

if [[ "$SKIP_FOZZY" -eq 0 ]]; then
  fozzy_trace_gate "$SCENARIO" "cua-smoke"
  fozzy_trace_gate "$RUNEBOOK_SCENARIO" "cua-runebook"
  fozzy_trace_gate "$SDK_SCENARIO" "cua-sdk-action"
  fozzy_trace_gate "$SCRATCHPAD_SCENARIO" "cua-scratchpad"
fi

log "Host production proofs"
run scripts/host-session-proof.sh | tee "$ARTIFACT_DIR/host-session-proof.json"
run scripts/host-control-surface-proof.sh | tee "$ARTIFACT_DIR/host-control-surface-proof.json"
run scripts/host-latency-proof.sh | tee "$ARTIFACT_DIR/host-latency-proof.json"
run scripts/host-machine-key-persistence-proof.sh | tee "$ARTIFACT_DIR/host-machine-key-persistence-proof.json"
run scripts/host-config-migration-proof.sh | tee "$ARTIFACT_DIR/host-config-migration-proof.json"
run scripts/host-sdk-session-proof.sh | tee "$ARTIFACT_DIR/host-sdk-session-proof.json"
run scripts/host-inbox-webhook-proof.sh | tee "$ARTIFACT_DIR/host-inbox-webhook-proof.json"
run scripts/host-scratchpad-proof.sh | tee "$ARTIFACT_DIR/host-scratchpad-proof.json"

log "Package app"
APP_PATH="$(scripts/package-macos-app.sh | tail -n 1)"
printf '%s\n' "$APP_PATH" | tee "$ARTIFACT_DIR/package-path.txt"
run scripts/host-package-proof.sh | tee "$ARTIFACT_DIR/host-package-proof.json"
run scripts/host-packaged-attestation-proof.sh | tee "$ARTIFACT_DIR/host-packaged-attestation-proof.json"

if [[ "$SKIP_INSTALL" -eq 0 ]]; then
  log "Install app"
  INSTALLED_PATH="$(scripts/install-macos-app.sh | tail -n 1)"
  printf '%s\n' "$INSTALLED_PATH" | tee "$ARTIFACT_DIR/install-path.txt"

  if [[ "$SKIP_RELAUNCH" -eq 0 ]]; then
    log "Relaunch installed app"
    restart_installed_app
  fi

  log "Installed app status"
  run "$INSTALL_APP/Contents/MacOS/cua" --profile default status --json | tee "$ARTIFACT_DIR/status.json"

  log "Installed input devices"
  run "$INSTALL_APP/Contents/MacOS/cua-voice" --list-input-devices | tee "$ARTIFACT_DIR/input-devices.json"

  if [[ "$SKIP_LIVE" -eq 0 ]]; then
    if openrouter_key_available; then
      log "Installed voice WAV smoke"
      WAV_PATH="$(generate_voice_fixture)"
      run "$INSTALL_APP/Contents/MacOS/cua-voice" \
        --headless \
        --profile default \
        --stt-backend "$STT_BACKEND" \
        --stt-model "$STT_MODEL" \
        --planner-model "$PLANNER_MODEL" \
        --once-wav "$WAV_PATH" | tee "$ARTIFACT_DIR/voice-wav-smoke.jsonl"
    else
      printf 'OPENROUTER_API_KEY not found; skipping installed voice WAV smoke\n' >&2
    fi
  fi
fi

log "Release complete"
printf 'artifacts=%s\n' "$ARTIFACT_DIR"
