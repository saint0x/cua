#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v jq >/dev/null

cargo build -p cua >/dev/null

debug_bin() {
  local name="$1"
  local target_root="${CARGO_TARGET_DIR:-target}"
  if [[ -x "$target_root/debug/$name" ]]; then
    printf '%s\n' "$target_root/debug/$name"
  elif [[ -x "target/debug/$name" ]]; then
    printf '%s\n' "target/debug/$name"
  else
    local root found
    for root in "$target_root" target; do
      [[ -d "$root" ]] || continue
      found="$(find "$root" -path "*/debug/$name" -type f 2>/dev/null | head -n 1 || true)"
      if [[ -n "$found" ]]; then
        printf '%s\n' "$found"
        return 0
      fi
    done
  fi
}

if [[ -n "${CUA_BIN:-}" ]]; then
  CUA_BIN_PATH="$CUA_BIN"
else
  CUA_BIN_PATH="$(debug_bin cua)"
fi

if [[ -z "${CUA_BIN_PATH:-}" || ! -x "$CUA_BIN_PATH" ]]; then
  cargo build -p cua
  CUA_BIN_PATH="${CUA_BIN:-$(debug_bin cua)}"
fi
if [[ ! -x "$CUA_BIN_PATH" ]]; then
  echo "cua binary not found" >&2
  exit 1
fi

RUN_ID="$(date +%s)"
OUT_DIR="${CUA_CONFIG_MIGRATION_PROOF_OUT_DIR:-artifacts/cua/config-migration-proof-$RUN_ID}"
PROOF="$OUT_DIR/proof.json"
mkdir -p "$OUT_DIR"

for state in missing current legacy conflict; do
  home="$OUT_DIR/$state-home"
  profile="proof-$state"
  addr="127.0.0.1:$((31000 + RUN_ID % 1000))"
  mkdir -p "$home"
  case "$state" in
    current)
      mkdir -p "$home/config"
      printf 'OPENROUTER_API_KEY=current-secret\n' > "$home/config/env"
      ;;
    legacy)
      printf 'OPENROUTER_API_KEY=legacy-secret\n' > "$home/.env"
      ;;
    conflict)
      mkdir -p "$home/config"
      printf 'OPENROUTER_API_KEY=current-secret\n' > "$home/config/env"
      printf 'OPENROUTER_API_KEY=legacy-secret\n' > "$home/.env"
      ;;
  esac

  CUA_HOME="$home" CUA_HTTP_TOKEN="config-proof-$state" CUA_HUD_AUTOSTART=0 \
    "$CUA_BIN_PATH" --profile "$profile" serve --addr "$addr" --hud-mode headless \
    > "$OUT_DIR/$state-daemon.log" 2>&1 &
  pid="$!"
  socket="$home/profiles/$profile/daemon.sock"
  cleanup_state() {
    kill "$pid" >/dev/null 2>&1 || true
    wait "$pid" >/dev/null 2>&1 || true
  }
  trap cleanup_state EXIT
  for _ in $(seq 1 100); do
    if [[ -S "$socket" ]] && CUA_HOME="$home" CUA_HTTP_TOKEN="config-proof-$state" \
      "$CUA_BIN_PATH" --profile "$profile" config status --json > "$OUT_DIR/$state.json" 2>/dev/null; then
      break
    fi
    sleep 0.05
  done
  CUA_HOME="$home" CUA_HTTP_TOKEN="config-proof-$state" \
    "$CUA_BIN_PATH" --profile "$profile" config status --json > "$OUT_DIR/$state.json"
  cleanup_state
  trap - EXIT
done

jq -n \
  --slurpfile missing "$OUT_DIR/missing.json" \
  --slurpfile current "$OUT_DIR/current.json" \
  --slurpfile legacy "$OUT_DIR/legacy.json" \
  --slurpfile conflict "$OUT_DIR/conflict.json" \
  '{
    schema_version: "cua.config_migration_proof.v1",
    ok: (
      $missing[0].migration_state == "missing" and
      $current[0].migration_state == "current" and
      $legacy[0].migration_state == "legacy_only" and
      $conflict[0].migration_state == "conflict" and
      ($current[0] | tostring | contains("current-secret") | not) and
      ($legacy[0] | tostring | contains("legacy-secret") | not) and
      ($conflict[0] | tostring | contains("legacy-secret") | not)
    ),
    states: {
      missing: $missing[0].migration_state,
      current: $current[0].migration_state,
      legacy: $legacy[0].migration_state,
      conflict: $conflict[0].migration_state
    },
    current_paths: {
      config_env: $current[0].config_env,
      legacy_config_env: $current[0].legacy_config_env,
      profile_socket: $current[0].profile_socket,
      artifact_root: $current[0].artifact_root
    }
  }' > "$PROOF"

jq -e '.ok == true' "$PROOF" >/dev/null
cat "$PROOF"
