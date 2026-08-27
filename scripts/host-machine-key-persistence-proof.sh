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
OUT_DIR="${CUA_MACHINE_KEY_PROOF_OUT_DIR:-artifacts/cua/machine-key-proof-$RUN_ID}"
CUA_HOME_DIR="$OUT_DIR/cua-home"
PROOF="$OUT_DIR/proof.json"
mkdir -p "$OUT_DIR"

CUA_HOME="$CUA_HOME_DIR" "$CUA_BIN_PATH" identity status --audience quilt-cloud --json > "$OUT_DIR/identity-first.json"
CUA_HOME="$CUA_HOME_DIR" "$CUA_BIN_PATH" identity status --audience quilt-cloud --json > "$OUT_DIR/identity-second.json"
CUA_HOME="$CUA_HOME_DIR" "$CUA_BIN_PATH" identity rotate --audience quilt-cloud --json > "$OUT_DIR/identity-rotated.json"
CUA_HOME="$CUA_HOME_DIR" "$CUA_BIN_PATH" identity status --audience quilt-cloud --json > "$OUT_DIR/identity-after-rotate.json"

jq -n \
  --slurpfile first "$OUT_DIR/identity-first.json" \
  --slurpfile second "$OUT_DIR/identity-second.json" \
  --slurpfile rotated "$OUT_DIR/identity-rotated.json" \
  --slurpfile after "$OUT_DIR/identity-after-rotate.json" \
  --arg cua_home "$CUA_HOME_DIR" \
  '{
    schema_version: "cua.machine_key_persistence_proof.v1",
    ok: (
      $first[0].identity.machine_key_id == $second[0].identity.machine_key_id and
      $first[0].identity.machine_public_key == $second[0].identity.machine_public_key and
      $rotated[0].identity.machine_key_id != $first[0].identity.machine_key_id and
      $after[0].identity.machine_key_id == $rotated[0].identity.machine_key_id and
      ($after[0].metadata_path | startswith($cua_home)) and
      ($after[0].key_path | startswith($cua_home)) and
      ($after[0].previous_keys_dir | startswith($cua_home))
    ),
    first_key_id: $first[0].identity.machine_key_id,
    rotated_key_id: $rotated[0].identity.machine_key_id,
    key_backend: $after[0].identity.key_backend,
    metadata_path: $after[0].metadata_path,
    key_path: $after[0].key_path,
    previous_keys_dir: $after[0].previous_keys_dir
  }' > "$PROOF"

jq -e '.ok == true' "$PROOF" >/dev/null
cat "$PROOF"
