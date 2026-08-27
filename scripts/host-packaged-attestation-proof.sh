#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v jq >/dev/null

RUN_ID="$(date +%s)"
OUT_DIR="${CUA_PACKAGED_ATTESTATION_PROOF_OUT_DIR:-artifacts/cua/packaged-attestation-proof-$RUN_ID}"
PACKAGE_OUT_DIR="$OUT_DIR/package"
CUA_HOME_DIR="${CUA_PACKAGED_ATTESTATION_PROOF_CUA_HOME:-/tmp/cua-pkg-attest-$RUN_ID}"
PROFILE="${CUA_PACKAGED_ATTESTATION_PROOF_PROFILE:-packaged-attestation-proof-$RUN_ID}"
ADDR="${CUA_PACKAGED_ATTESTATION_PROOF_ADDR:-127.0.0.1:$((33000 + RUN_ID % 1000))}"
TOKEN="${CUA_PACKAGED_ATTESTATION_PROOF_TOKEN:-packaged-attestation-proof-token-$RUN_ID}"
PROOF="$OUT_DIR/proof.json"
mkdir -p "$OUT_DIR"

CUA_PACKAGE_PROOF_OUT_DIR="$PACKAGE_OUT_DIR" scripts/host-package-proof.sh > "$OUT_DIR/package-proof.stdout"
PACKAGE_PROOF="$PACKAGE_OUT_DIR/proof.json"
APP_PATH="$(jq -r '.app_path' "$PACKAGE_PROOF")"
CUA_BIN_PATH="$APP_PATH/Contents/MacOS/cua"

if [[ ! -x "$CUA_BIN_PATH" ]]; then
  echo "packaged cua binary not found at $CUA_BIN_PATH" >&2
  exit 1
fi

CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" CUA_HUD_AUTOSTART=0 \
  "$CUA_BIN_PATH" --profile "$PROFILE" serve --addr "$ADDR" --hud-mode headless \
  > "$OUT_DIR/daemon.log" 2>&1 &
PID="$!"

cleanup() {
  kill "$PID" >/dev/null 2>&1 || true
  wait "$PID" >/dev/null 2>&1 || true
}
trap cleanup EXIT

SOCKET="$CUA_HOME_DIR/profiles/$PROFILE/daemon.sock"
for _ in $(seq 1 120); do
  if [[ -S "$SOCKET" ]] && CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" \
    "$CUA_BIN_PATH" --profile "$PROFILE" status --json > "$OUT_DIR/status.json" 2>/dev/null; then
    break
  fi
  sleep 0.05
done
CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" \
  "$CUA_BIN_PATH" --profile "$PROFILE" status --json > "$OUT_DIR/status.json"

CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" \
  "$CUA_BIN_PATH" --profile "$PROFILE" attestation challenge --audience quilt-cloud --json \
  > "$OUT_DIR/challenge.json"
NONCE="$(jq -r '.nonce' "$OUT_DIR/challenge.json")"
CHALLENGE_ID="$(jq -r '.challenge_id' "$OUT_DIR/challenge.json")"
CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" \
  "$CUA_BIN_PATH" --profile "$PROFILE" attestation sign \
    --audience quilt-cloud \
    --nonce "$NONCE" \
    --challenge-id "$CHALLENGE_ID" \
    --json > "$OUT_DIR/attestation.json"
CUA_HOME="$CUA_HOME_DIR" CUA_HTTP_TOKEN="$TOKEN" \
  "$CUA_BIN_PATH" --profile "$PROFILE" attestation verify "$OUT_DIR/attestation.json" \
    --audience quilt-cloud \
    --json > "$OUT_DIR/verify.json"

jq -n \
  --slurpfile package "$PACKAGE_PROOF" \
  --slurpfile status "$OUT_DIR/status.json" \
  --slurpfile challenge "$OUT_DIR/challenge.json" \
  --slurpfile attestation "$OUT_DIR/attestation.json" \
  --slurpfile verify "$OUT_DIR/verify.json" \
  '{
    schema_version: "cua.packaged_attestation_proof.v1",
    ok: (
      $package[0].ok == true and
      $verify[0].accepted == true and
      $verify[0].reason == "ok" and
      $attestation[0].challenge.challenge_id == $challenge[0].challenge_id and
      $attestation[0].challenge.audience == "quilt-cloud" and
      $attestation[0].claims.runtime_name == "cua" and
      ($attestation[0].claims.runtime_version | length) > 0 and
      ($attestation[0].claims.daemon_pid | type) == "number" and
      ($attestation[0].claims.socket_path | length) > 0 and
      ($attestation[0].claims.http_addr | length) > 0 and
      $attestation[0].claims.bundle_id == "io.saint0x.cua" and
      ($attestation[0].claims.code_signature_summary | contains("Authority=")) and
      ($attestation[0].claims.designated_requirement | contains("identifier \"io.saint0x.cua\"")) and
      ($attestation[0].claims.binary_sha256 | length) > 0 and
      ($attestation[0].signature | length) > 0 and
      ($package[0].signature | contains("Authority=")) and
      ($package[0].designated_requirement | contains("identifier \"io.saint0x.cua\""))
    ),
    app_path: $package[0].app_path,
    bundle_id: $package[0].bundle_id,
    executable: $package[0].executable,
    challenge_id: $challenge[0].challenge_id,
    machine_key_id: $attestation[0].identity.machine_key_id,
    runtime_claims: $attestation[0].claims,
    verify: $verify[0],
    package_signature: $package[0].signature,
    designated_requirement: $package[0].designated_requirement,
    status_profile: $status[0].profile
  }' > "$PROOF"

jq -e '.ok == true' "$PROOF" >/dev/null
cat "$PROOF"
