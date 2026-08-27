# Config Home Policy

cua stores durable runtime state under `~/.cua` unless `CUA_HOME` is set for tests or explicit development runs.

Canonical paths:

- `~/.cua/config/env`: production environment file loaded by the CLI and voice runtime
- `~/.cua/profiles/<profile>/http.token`: per-profile bearer token
- `~/.cua/profiles/<profile>/daemon.sock`: profile-local Unix socket
- `~/.cua/profiles/<profile>/chat.db`: local chat history
- `~/.cua/profiles/<profile>/ctx`: ctx workspace
- `~/.cua/bin/ctx`: local ctx install path for non-packaged production installs
- packaged sibling `ctx`: ctx path inside `cua.app/Contents/MacOS`

The current working directory `.env` is not loaded by production runtime code. Use `~/.cua/config/env` or set `CUA_ENV_FILE` explicitly.

`CUA_HTTP_TOKEN` is a development/test token override only. Production runtime auth uses the profile token file.

The repo-local `vendor/ctx/ctx` path is allowed only when `CUA_DEV_REPO_PATHS=1` or in tests. Packaged production resolves `ctx` as a sibling binary first, then `~/.cua/bin/ctx`.
