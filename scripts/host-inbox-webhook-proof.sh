#!/usr/bin/env bash
set -euo pipefail

RUN_ID="${CUA_INBOX_WEBHOOK_PROOF_RUN_ID:-$(date +%s)}"
PROFILE="${CUA_INBOX_WEBHOOK_PROOF_PROFILE:-inbox-webhook-proof-$RUN_ID}"
TOKEN="${CUA_INBOX_WEBHOOK_PROOF_TOKEN:-inbox-webhook-proof-token-$RUN_ID}"
CUA_HOME_DIR="${CUA_INBOX_WEBHOOK_PROOF_HOME:-$(mktemp -d "/tmp/cua-iw.XXXXXX")}"
OUT_DIR="${CUA_INBOX_WEBHOOK_PROOF_OUT_DIR:-artifacts/cua/inbox-webhook-proof-$RUN_ID}"

debug_bin() {
  local name="$1"
  local target_root="${CARGO_TARGET_DIR:-target}"
  local candidate
  for candidate in "$target_root/debug/$name" "$target_root/aarch64-apple-darwin/debug/$name" "$target_root/x86_64-apple-darwin/debug/$name"; do
    if [[ -x "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  find "$target_root" -path "*/debug/$name" -type f -perm -111 -print -quit
}

command -v jq >/dev/null
command -v curl >/dev/null
command -v python3 >/dev/null
cargo build -p cua >/dev/null

CUA_BIN_PATH="${CUA_BIN:-$(debug_bin cua)}"
if [[ -z "$CUA_BIN_PATH" || ! -x "$CUA_BIN_PATH" ]]; then
  echo "could not find cua debug binary" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"
PORT="$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)"
ADDR="127.0.0.1:$PORT"
LOG="$OUT_DIR/daemon.log"

CUA_HOME="$CUA_HOME_DIR" CUA_DEV_HTTP_TOKEN_OVERRIDE=1 CUA_HTTP_TOKEN="$TOKEN" \
  "$CUA_BIN_PATH" --profile "$PROFILE" serve --addr "$ADDR" --hud-mode headless >"$LOG" 2>&1 &
PID=$!
cleanup() {
  kill "$PID" >/dev/null 2>&1 || true
}
trap cleanup EXIT

for _ in $(seq 1 80); do
  if CUA_HOME="$CUA_HOME_DIR" CUA_DEV_HTTP_TOKEN_OVERRIDE=1 CUA_HTTP_TOKEN="$TOKEN" \
    "$CUA_BIN_PATH" --profile "$PROFILE" status --json >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done

CUA_HOME="$CUA_HOME_DIR" CUA_DEV_HTTP_TOKEN_OVERRIDE=1 CUA_HTTP_TOKEN="$TOKEN" \
  "$CUA_BIN_PATH" --profile "$PROFILE" inbox publish "check inbox proof" \
    --source host-proof --idempotency-key host-proof-cli --json >"$OUT_DIR/inbox-publish.json"

CUA_HOME="$CUA_HOME_DIR" CUA_DEV_HTTP_TOKEN_OVERRIDE=1 CUA_HTTP_TOKEN="$TOKEN" \
  "$CUA_BIN_PATH" --profile "$PROFILE" inbox wait --after 0 --json >"$OUT_DIR/inbox-wait.json"

curl -fsS \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"schema_version":"cua.v1","source":"alerts","shared_secret":"shared-proof-secret","reply_url":null}' \
  "http://$ADDR/webhooks/alerts/subscribe" >"$OUT_DIR/webhook-subscribe.json"

BODY="$OUT_DIR/webhook-body.json"
cat >"$BODY" <<'JSON'
{"schema_version":"cua.v1","idempotency_key":"host-proof-webhook","source":"alerts","text":"webhook proof message","payload":{"severity":"low"}}
JSON
SIG="$(python3 - "$BODY" <<'PY'
import hashlib, hmac, pathlib, sys
body = pathlib.Path(sys.argv[1]).read_bytes()
print("sha256=" + hmac.new(b"shared-proof-secret", body, hashlib.sha256).hexdigest())
PY
)"
curl -fsS \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -H "x-cua-webhook-signature: $SIG" \
  --data-binary @"$BODY" \
  "http://$ADDR/webhooks/alerts" >"$OUT_DIR/webhook-publish.json"

MESSAGE_ID="$(jq -r '.message_id' "$OUT_DIR/webhook-publish.json")"
CUA_HOME="$CUA_HOME_DIR" CUA_DEV_HTTP_TOKEN_OVERRIDE=1 CUA_HTTP_TOKEN="$TOKEN" \
  "$CUA_BIN_PATH" --profile "$PROFILE" inbox status "$MESSAGE_ID" --json >"$OUT_DIR/webhook-status.json"

jq -n \
  --slurpfile inbox "$OUT_DIR/inbox-publish.json" \
  --slurpfile waited "$OUT_DIR/inbox-wait.json" \
  --slurpfile subscription "$OUT_DIR/webhook-subscribe.json" \
  --slurpfile webhook "$OUT_DIR/webhook-publish.json" \
  --slurpfile status "$OUT_DIR/webhook-status.json" \
  '{
    schema_version: "cua.inbox_webhook_proof.v1",
    ok: (
      $inbox[0].state == "accepted" and
      ($waited[0] | length) >= 1 and
      $subscription[0].configured == true and
      $subscription[0].requires_signature == true and
      $webhook[0].state == "accepted" and
      $webhook[0].message.delivery_method == "webhook" and
      $status[0].message.text == "webhook proof message"
    ),
    profile: $inbox[0].message.source,
    cli_message_id: $inbox[0].message_id,
    webhook_message_id: $webhook[0].message_id,
    artifacts: {
      out_dir: "'"$OUT_DIR"'",
      cua_home: "'"$CUA_HOME_DIR"'",
      daemon_log: "'"$LOG"'"
    }
  }'
