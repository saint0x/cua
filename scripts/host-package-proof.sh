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

BUNDLE_ID="$(plutil -extract CFBundleIdentifier raw -o - "$INFO_PLIST")"
EXECUTABLE="$(plutil -extract CFBundleExecutable raw -o - "$INFO_PLIST")"
LSUI_ELEMENT="$(plutil -extract LSUIElement raw -o - "$INFO_PLIST")"
MIC_USAGE="$(plutil -extract NSMicrophoneUsageDescription raw -o - "$INFO_PLIST")"
INPUT_USAGE="$(plutil -extract NSInputMonitoringUsageDescription raw -o - "$INFO_PLIST")"
AUTOMATION_USAGE="$(plutil -extract NSAppleEventsUsageDescription raw -o - "$INFO_PLIST")"

jq -n \
  --arg app_path "$APP_PATH" \
  --arg bundle_id "$BUNDLE_ID" \
  --arg executable "$EXECUTABLE" \
  --arg lsui_element "$LSUI_ELEMENT" \
  --arg microphone_usage "$MIC_USAGE" \
  --arg input_monitoring_usage "$INPUT_USAGE" \
  --arg automation_usage "$AUTOMATION_USAGE" \
  --arg signature "$SIGNATURE" \
  --arg designated_requirement "$DESIGNATED_REQUIREMENT" \
  '{
    schema_version: "cua.package_proof.v1",
    ok: (
      $bundle_id == "io.saint0x.cua" and
      ($app_path | endswith("/cua.app")) and
      $executable == "cua-voice" and
      ($lsui_element == "1" or $lsui_element == "true") and
      ($microphone_usage | length) > 0 and
      ($input_monitoring_usage | length) > 0 and
      ($automation_usage | length) > 0 and
      ($signature | contains("Signature=adhoc") | not) and
      ($signature | contains("Authority=")) and
      ($designated_requirement | contains("identifier \"io.saint0x.cua\"")) and
      ($designated_requirement | contains("cdhash") | not)
    ),
    app_path: $app_path,
    bundle_id: $bundle_id,
    executable: $executable,
    lsui_element: $lsui_element,
    usage_descriptions: {
      microphone: $microphone_usage,
      input_monitoring: $input_monitoring_usage,
      automation: $automation_usage
    },
    signature: $signature,
    designated_requirement: $designated_requirement,
    binaries: ["cua", "cua-voice"]
  }' > "$PROOF"

jq -e '.ok == true' "$PROOF" >/dev/null

echo "$OUT_DIR"
