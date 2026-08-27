#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="${CUA_APP_NAME:-cua}"
INSTALL_DIR="${CUA_APP_INSTALL_DIR:-$HOME/Applications}"
INSTALL_APP="$INSTALL_DIR/$APP_NAME.app"
RUN_ID="$(date +%Y%m%d-%H%M%S)"
ARTIFACT_DIR="${CUA_RELEASE_ARTIFACT_DIR:-$ROOT/artifacts/cua/release/$RUN_ID}"
SEED="${CUA_RELEASE_SEED:-424242}"
SCENARIO="${CUA_RELEASE_SCENARIO:-fozzy/scenarios/cua-smoke.json}"
STT_BACKEND="${CUA_RELEASE_STT_BACKEND:-local}"
STT_MODEL="${CUA_RELEASE_STT_MODEL:-tiny.en}"
PLANNER_MODEL="${CUA_RELEASE_PLANNER_MODEL:-google/gemini-2.5-flash-lite}"
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
  CUA_RELEASE_PLANNER_MODEL Planner model for live smoke. Default: google/gemini-2.5-flash-lite.
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
  grep -Eq '^[[:space:]]*(export[[:space:]]+)?OPENROUTER_API_KEY=' "$ROOT/.env" 2>/dev/null && return 0
  grep -Eq '^[[:space:]]*(export[[:space:]]+)?OPENROUTER_API_KEY=' "$HOME/.cua/.env" 2>/dev/null && return 0
  return 1
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

log "Release config"
printf 'artifact_dir=%s\n' "$ARTIFACT_DIR"
printf 'stt_backend=%s\n' "$STT_BACKEND"
printf 'stt_model=%s\n' "$STT_MODEL"
printf 'planner_model=%s\n' "$PLANNER_MODEL"

if [[ "$SKIP_TESTS" -eq 0 ]]; then
  log "Format and compile checks"
  run cargo fmt --check
  run git diff --check
  run cargo check -p cua-voice --all-targets

  log "Unit tests"
  run cargo test -p cua-voice --lib
  run cargo test -p cua-voice --bin cua-voice
fi

if [[ "$SKIP_FOZZY" -eq 0 ]]; then
  if command -v fozzy >/dev/null 2>&1; then
    log "Fozzy deterministic smoke"
    run fozzy doctor --deep --scenario "$SCENARIO" --runs 5 --seed "$SEED" --strict --host-backends --json
    run fozzy test "$SCENARIO" --det --strict-verify --seed "$SEED" --host-backends --json
    TRACE="$ARTIFACT_DIR/cua-smoke.fozzy"
    run fozzy run "$SCENARIO" --det --record "$TRACE" --seed "$SEED" --host-backends --json
    run fozzy trace verify "$TRACE" --strict --json
    run fozzy replay "$TRACE" --json
    run fozzy ci "$TRACE" --strict --json
  else
    printf 'fozzy not found; skipping deterministic smoke\n' >&2
  fi
fi

log "Package app"
APP_PATH="$(scripts/package-macos-app.sh | tail -n 1)"
printf '%s\n' "$APP_PATH" | tee "$ARTIFACT_DIR/package-path.txt"

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
