#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v afconvert >/dev/null
command -v curl >/dev/null
command -v jq >/dev/null
command -v perl >/dev/null
command -v say >/dev/null

if [[ -z "${OPENROUTER_API_KEY:-}" && -f .env ]]; then
  while IFS='=' read -r key value; do
    key="${key#export }"
    if [[ "$key" == "OPENROUTER_API_KEY" && -n "${value:-}" ]]; then
      value="${value%$'\r'}"
      value="${value%\"}"
      value="${value#\"}"
      value="${value%\'}"
      value="${value#\'}"
      export OPENROUTER_API_KEY="$value"
      break
    fi
  done < .env
fi

if [[ -z "${OPENROUTER_API_KEY:-}" ]]; then
  echo "OPENROUTER_API_KEY is required for host voice WAV proof" >&2
  exit 1
fi

RUN_ID="$(date +%s)"
PROFILE="${CUA_VOICE_WAV_PROOF_PROFILE:-host-voice-wav-proof-$RUN_ID}"
ADDR="${CUA_VOICE_WAV_PROOF_ADDR:-127.0.0.1:9881}"
TOKEN="${CUA_HTTP_TOKEN:-host-voice-wav-proof-token-$RUN_ID}"
OUT_DIR="${CUA_VOICE_WAV_PROOF_OUT_DIR:-artifacts/cua/voice-wav-proof-$RUN_ID}"
TRACE_DIR="$OUT_DIR/trace"
AIFF="$OUT_DIR/pause.aiff"
WAV="$OUT_DIR/pause.wav"
EVENTS="$OUT_DIR/events.jsonl"
STATUS="$OUT_DIR/status.json"
PROOF="$OUT_DIR/proof.json"

cargo build -p cua -p cua-voice

if [[ -n "${CUA_BIN:-}" ]]; then
  CUA_BIN_PATH="$CUA_BIN"
elif [[ -x target/debug/cua ]]; then
  CUA_BIN_PATH="target/debug/cua"
else
  CUA_BIN_PATH="$(find target -path '*/debug/cua' -type f | head -n 1)"
fi

if [[ -n "${CUA_VOICE_BIN:-}" ]]; then
  VOICE_BIN_PATH="$CUA_VOICE_BIN"
elif [[ -x target/debug/cua-voice ]]; then
  VOICE_BIN_PATH="target/debug/cua-voice"
else
  VOICE_BIN_PATH="$(find target -path '*/debug/cua-voice' -type f | head -n 1)"
fi

if [[ -z "$CUA_BIN_PATH" || ! -x "$CUA_BIN_PATH" ]]; then
  echo "cua binary not found" >&2
  exit 1
fi
if [[ -z "$VOICE_BIN_PATH" || ! -x "$VOICE_BIN_PATH" ]]; then
  echo "cua-voice binary not found" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"
say -o "$AIFF" "pause"
afconvert -f WAVE -d LEI16@16000 "$AIFF" "$WAV"

CUA_HTTP_TOKEN="$TOKEN" CUA_TRACE_DIR="$TRACE_DIR" "$CUA_BIN_PATH" \
  --server-addr "$ADDR" \
  --profile "$PROFILE" \
  serve --addr "$ADDR" &
DAEMON_PID="$!"

cleanup() {
  kill "$DAEMON_PID" >/dev/null 2>&1 || true
  wait "$DAEMON_PID" >/dev/null 2>&1 || true
}
trap cleanup EXIT

for _ in $(seq 1 80); do
  if curl -fs "http://$ADDR/healthz" >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done
curl -fsS "http://$ADDR/healthz" >/dev/null

START_MS="$(perl -MTime::HiRes=time -e 'printf "%.0f\n", time() * 1000')"
CUA_HTTP_TOKEN="$TOKEN" OPENROUTER_API_KEY="$OPENROUTER_API_KEY" "$VOICE_BIN_PATH" \
  --profile "$PROFILE" \
  --once-wav "$WAV" > "$EVENTS"
END_MS="$(perl -MTime::HiRes=time -e 'printf "%.0f\n", time() * 1000')"
VOICE_ELAPSED_MS="$((END_MS - START_MS))"

CUA_HTTP_TOKEN="$TOKEN" "$CUA_BIN_PATH" \
  --server-addr "$ADDR" \
  --profile "$PROFILE" \
  status --json > "$STATUS"

jq -s -e '
  any(.event == "transcribing") and
  any(.event == "transcript" and (.text | ascii_downcase | contains("pause"))) and
  any(.event == "planning" and .tool == "Command parser") and
  any(.event == "dispatching" and (.action | contains("Pause"))) and
  any(.event == "reply" and (.text | ascii_downcase | contains("paused")) and (.text | ascii_downcase | contains("confirmed")))
' "$EVENTS" >/dev/null

jq -e '
  .safety_state == "paused"
' "$STATUS" >/dev/null

jq -n \
  --arg profile "$PROFILE" \
  --arg wav "$WAV" \
  --argjson elapsed_ms "$VOICE_ELAPSED_MS" \
  --slurpfile events "$EVENTS" \
  --slurpfile status "$STATUS" \
  '{
    schema_version: "cua.voice_proof.v1",
    profile: $profile,
    wav: $wav,
    elapsed_ms: $elapsed_ms,
    events: ($events | map(.event)),
    transcript: (($events | map(select(.event == "transcript")) | first).text),
    dispatch: (($events | map(select(.event == "dispatching")) | first).action),
    reply: (($events | map(select(.event == "reply")) | first).text),
    safety_state: $status[0].safety_state,
    active_profile: $status[0].active_profile
  }' > "$PROOF"

echo "$OUT_DIR"
