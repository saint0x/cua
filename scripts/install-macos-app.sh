#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="${CUA_APP_NAME:-cua}"
BUNDLE_ID="${CUA_BUNDLE_ID:-io.saint0x.cua}"
SOURCE_APP="${CUA_APP_SOURCE:-$ROOT/artifacts/cua/macos/$APP_NAME.app}"
INSTALL_DIR="${CUA_APP_INSTALL_DIR:-$HOME/Applications}"
INSTALL_APP="$INSTALL_DIR/$APP_NAME.app"
LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"

sync_env_secret() {
  local name="$1"
  local env_dir="${CUA_HOME:-$HOME/.cua}/config"
  local env_file="$env_dir/env"
  local value="${!name:-}"
  [[ -n "$value" ]] || return 0
  mkdir -p "$env_dir"
  chmod 700 "$env_dir"
  touch "$env_file"
  chmod 600 "$env_file"
  local tmp
  tmp="$(mktemp "$env_dir/env.XXXXXX")"
  grep -v "^[[:space:]]*\\(export[[:space:]]\\+\\)\\{0,1\\}${name}=" "$env_file" > "$tmp" || true
  local escaped="$value"
  escaped="${escaped//\\/\\\\}"
  escaped="${escaped//\"/\\\"}"
  printf '%s="%s"\n' "$name" "$escaped" >> "$tmp"
  chmod 600 "$tmp"
  mv "$tmp" "$env_file"
}

sync_planner_env() {
  sync_env_secret OPENROUTER_API_KEY
  sync_env_secret GEMINI_API_KEY
  sync_env_secret GOOGLE_API_KEY
}

if [[ -z "${CUA_APP_SOURCE:-}" ]]; then
  "$ROOT/scripts/package-macos-app.sh" >/dev/null
elif [[ ! -d "$SOURCE_APP" ]]; then
  "$ROOT/scripts/package-macos-app.sh" >/dev/null
fi

if [[ ! -d "$SOURCE_APP" ]]; then
  printf 'missing packaged app: %s\n' "$SOURCE_APP" >&2
  exit 1
fi

mkdir -p "$INSTALL_DIR"
find "$INSTALL_DIR" -maxdepth 1 -type d -iname "$APP_NAME.app" ! -name "$APP_NAME.app" -exec rm -rf {} +
rm -rf "$INSTALL_APP"
ditto "$SOURCE_APP" "$INSTALL_APP"
sync_planner_env

if [[ -x "$LSREGISTER" ]]; then
  while IFS= read -r registered_app; do
    [[ "$registered_app" == "$SOURCE_APP" ]] && continue
    [[ "$registered_app" == "$INSTALL_APP" ]] && continue
    "$LSREGISTER" -u "$registered_app" >/dev/null 2>&1 || true
  done < <("$LSREGISTER" -dump | awk -v bundle_id="$BUNDLE_ID" '
    /^[[:space:]]*path:/ {
      path = $0
      sub(/^[[:space:]]*path:[[:space:]]*/, "", path)
      sub(/[[:space:]]+\(0x[0-9a-fA-F]+\).*$/, "", path)
    }
    /^[[:space:]]*identifier:/ {
      identifier = $0
      sub(/^[[:space:]]*identifier:[[:space:]]*/, "", identifier)
      if (identifier == bundle_id && path != "") {
        print path
      }
    }
  ')
  while IFS= read -r stale_app; do
    [[ "$stale_app" == "$SOURCE_APP" ]] && continue
    [[ "$stale_app" == "$INSTALL_APP" ]] && continue
    "$LSREGISTER" -u "$stale_app" >/dev/null 2>&1 || true
  done < <(find "$ROOT/artifacts/cua" -path "*/$APP_NAME.app" -type d 2>/dev/null)
  "$LSREGISTER" -f "$INSTALL_APP"
fi

printf '%s\n' "$INSTALL_APP"
