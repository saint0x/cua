#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="${CUA_APP_NAME:-cua}"
BUNDLE_ID="${CUA_BUNDLE_ID:-io.saint0x.cua}"
SIGN_IDENTITY="${CUA_CODESIGN_IDENTITY:-}"
OUT_DIR="${CUA_APP_OUT_DIR:-$ROOT/artifacts/cua/macos}"
APP_DIR="$OUT_DIR/$APP_NAME.app"
CONTENTS_DIR="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
RESOURCES_DIR="$CONTENTS_DIR/Resources"
ENTITLEMENTS="$OUT_DIR/$APP_NAME.entitlements.plist"

cargo build -p cua --release
cargo build -p cua-voice --release

BIN="$ROOT/target/release/cua"
VOICE_BIN="$ROOT/target/release/cua-voice"
if [[ ! -x "$BIN" ]]; then
  CANDIDATES=()
  while IFS= read -r candidate; do
    CANDIDATES+=("$candidate")
  done < <(find "$ROOT/target" -path '*/release/cua' -type f -perm +111 | sort)
  if [[ "${#CANDIDATES[@]}" -ne 1 ]]; then
    printf 'expected one release cua binary, found %s\n' "${#CANDIDATES[@]}" >&2
    exit 1
  fi
  BIN="${CANDIDATES[0]}"
fi
if [[ ! -x "$VOICE_BIN" ]]; then
  VOICE_CANDIDATES=()
  while IFS= read -r candidate; do
    VOICE_CANDIDATES+=("$candidate")
  done < <(find "$ROOT/target" -path '*/release/cua-voice' -type f -perm +111 | sort)
  if [[ "${#VOICE_CANDIDATES[@]}" -ne 1 ]]; then
    printf 'expected one release cua-voice binary, found %s\n' "${#VOICE_CANDIDATES[@]}" >&2
    exit 1
  fi
  VOICE_BIN="${VOICE_CANDIDATES[0]}"
fi

rm -rf "$APP_DIR"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"
install -m 0755 "$BIN" "$MACOS_DIR/cua"
install -m 0755 "$VOICE_BIN" "$MACOS_DIR/cua-voice"

cat > "$CONTENTS_DIR/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleDisplayName</key>
  <string>$APP_NAME</string>
  <key>CFBundleExecutable</key>
  <string>cua-voice</string>
  <key>CFBundleIdentifier</key>
  <string>$BUNDLE_ID</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>$APP_NAME</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>0.1.0</string>
  <key>CFBundleVersion</key>
  <string>0.1.0</string>
  <key>LSApplicationCategoryType</key>
  <string>public.app-category.developer-tools</string>
  <key>LSMinimumSystemVersion</key>
  <string>14.0</string>
  <key>LSUIElement</key>
  <true/>
  <key>NSQuitAlwaysKeepsWindows</key>
  <false/>
  <key>NSAppleEventsUsageDescription</key>
  <string>cua needs local automation permission when a supervised profile grants desktop actions.</string>
  <key>NSMicrophoneUsageDescription</key>
  <string>cua uses microphone input only when the voice HUD records a requested command.</string>
  <key>NSScreenCaptureUsageDescription</key>
  <string>cua captures the local screen only when a supervised profile requests desktop observation.</string>
  <key>NSInputMonitoringUsageDescription</key>
  <string>cua listens for a local double-Control shortcut to start voice recording.</string>
</dict>
</plist>
PLIST

plutil -extract NSMicrophoneUsageDescription raw -o - "$CONTENTS_DIR/Info.plist" >/dev/null
plutil -extract NSScreenCaptureUsageDescription raw -o - "$CONTENTS_DIR/Info.plist" >/dev/null
plutil -extract NSInputMonitoringUsageDescription raw -o - "$CONTENTS_DIR/Info.plist" >/dev/null

cat > "$ENTITLEMENTS" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>com.apple.security.cs.disable-library-validation</key>
  <true/>
</dict>
</plist>
PLIST

if [[ -z "$SIGN_IDENTITY" ]]; then
  SIGN_IDENTITY="$("$ROOT/scripts/ensure-macos-codesign-identity.sh")"
fi

/usr/bin/codesign \
  --force \
  --sign "$SIGN_IDENTITY" \
  --identifier "$BUNDLE_ID" \
  --options runtime \
  --timestamp=none \
  "$MACOS_DIR/cua"

/usr/bin/codesign \
  --force \
  --sign "$SIGN_IDENTITY" \
  --identifier "$BUNDLE_ID" \
  --options runtime \
  --timestamp=none \
  "$MACOS_DIR/cua-voice"

/usr/bin/codesign \
  --force \
  --sign "$SIGN_IDENTITY" \
  --entitlements "$ENTITLEMENTS" \
  --options runtime \
  --timestamp=none \
  "$APP_DIR"

/usr/bin/codesign --verify --deep --strict --verbose=2 "$APP_DIR"
/usr/bin/codesign --display --verbose=2 "$APP_DIR"

LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
if [[ -x "$LSREGISTER" ]]; then
  "$LSREGISTER" -f "$APP_DIR"
fi

echo "$APP_DIR"
