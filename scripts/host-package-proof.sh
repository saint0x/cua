#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v jq >/dev/null
command -v plutil >/dev/null

RUN_ID="$(date +%s)"
OUT_DIR="${CUA_PACKAGE_PROOF_OUT_DIR:-artifacts/cua/package-proof-$RUN_ID}"
APP_OUT_DIR="$OUT_DIR/app"
PROOF="$OUT_DIR/proof.json"
mkdir -p "$OUT_DIR"

APP_PATH="$(
  CUA_APP_OUT_DIR="$APP_OUT_DIR" \
  scripts/package-macos-app.sh | tail -n 1
)"

INFO_PLIST="$APP_PATH/Contents/Info.plist"
MACOS_DIR="$APP_PATH/Contents/MacOS"

test -x "$MACOS_DIR/cua"
test -x "$MACOS_DIR/cua-voice"
test ! -e "$MACOS_DIR/cua-app"
/usr/bin/codesign --verify --deep --strict "$APP_PATH" >/dev/null
SIGNATURE="$(/usr/bin/codesign --display --verbose=2 "$APP_PATH" 2>&1)"
DESIGNATED_REQUIREMENT="$(/usr/bin/codesign -d -r- "$APP_PATH" 2>&1)"
DAEMON_SIGNATURE="$(/usr/bin/codesign --display --verbose=2 "$MACOS_DIR/cua" 2>&1)"
VOICE_SIGNATURE="$(/usr/bin/codesign --display --verbose=2 "$MACOS_DIR/cua-voice" 2>&1)"

BUNDLE_ID="$(plutil -extract CFBundleIdentifier raw -o - "$INFO_PLIST")"
EXECUTABLE="$(plutil -extract CFBundleExecutable raw -o - "$INFO_PLIST")"
LSUI_ELEMENT="$(plutil -extract LSUIElement raw -o - "$INFO_PLIST")"
MIC_USAGE="$(plutil -extract NSMicrophoneUsageDescription raw -o - "$INFO_PLIST")"
SCREEN_USAGE="$(plutil -extract NSScreenCaptureUsageDescription raw -o - "$INFO_PLIST")"
AUTOMATION_USAGE="$(plutil -extract NSAppleEventsUsageDescription raw -o - "$INFO_PLIST")"
if plutil -extract NSInputMonitoringUsageDescription raw -o - "$INFO_PLIST" >/dev/null 2>&1; then
  HAS_INPUT_MONITORING_USAGE="true"
else
  HAS_INPUT_MONITORING_USAGE="false"
fi

jq -n \
  --arg app_path "$APP_PATH" \
  --arg bundle_id "$BUNDLE_ID" \
  --arg executable "$EXECUTABLE" \
  --arg lsui_element "$LSUI_ELEMENT" \
  --arg microphone_usage "$MIC_USAGE" \
  --arg screen_capture_usage "$SCREEN_USAGE" \
  --arg has_input_monitoring_usage "$HAS_INPUT_MONITORING_USAGE" \
  --arg automation_usage "$AUTOMATION_USAGE" \
  --arg signature "$SIGNATURE" \
  --arg designated_requirement "$DESIGNATED_REQUIREMENT" \
  --arg daemon_signature "$DAEMON_SIGNATURE" \
  --arg voice_signature "$VOICE_SIGNATURE" \
  '{
    schema_version: "cua.package_proof.v1",
    ok: (
      $bundle_id == "io.saint0x.cua" and
      ($app_path | endswith("/cua.app")) and
      $executable == "cua-voice" and
      ($lsui_element == "0" or $lsui_element == "false") and
      ($microphone_usage | length) > 0 and
      ($screen_capture_usage | length) > 0 and
      $has_input_monitoring_usage == "false" and
      ($automation_usage | length) > 0 and
      ($signature | contains("Signature=adhoc") | not) and
      ($signature | contains("Authority=")) and
      ($daemon_signature | contains("Identifier=io.saint0x.cua")) and
      ($daemon_signature | contains("Info.plist entries=")) and
      ($voice_signature | contains("Identifier=io.saint0x.cua")) and
      ($voice_signature | contains("Info.plist entries=")) and
      ($designated_requirement | contains("identifier \"io.saint0x.cua\"")) and
      ($designated_requirement | contains("cdhash") | not)
    ),
    app_path: $app_path,
    bundle_id: $bundle_id,
    executable: $executable,
    lsui_element: $lsui_element,
    usage_descriptions: {
      microphone: $microphone_usage,
      screen_capture: $screen_capture_usage,
      input_monitoring: null,
      automation: $automation_usage
    },
    has_input_monitoring_usage: ($has_input_monitoring_usage == "true"),
    signature: $signature,
    daemon_signature: $daemon_signature,
    voice_signature: $voice_signature,
    designated_requirement: $designated_requirement,
    binaries: ["cua", "cua-voice"]
  }' > "$PROOF"

jq -e '.ok == true' "$PROOF" >/dev/null

echo "$OUT_DIR"
