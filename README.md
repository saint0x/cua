# cua

cua is a CLI-first local computer-use runtime for macOS agents. It provides one resident daemon, a profile-local Unix socket for hot control paths, a loopback HTTP API for operator access, typed desktop observation, action delivery results, trace artifacts, and bounded model evals for selecting vision/action models without turning the project into an agent framework.

On macOS, the shipped product is one Dock-visible app bundle: `cua.app`. The app opens the compact dynamic-island HUD and starts the local runtime it needs; the CLI and HTTP surfaces remain available for agents and operators.

Public control surfaces are intentionally limited to:

- `cua` CLI
- profile-local Unix socket protocol
- loopback local HTTP API

## Structure

- `crates/cua-core`: shared protocol types and JSON schema bundle
- `crates/cua-cli`: human-facing CLI
- `crates/cua-daemon`: resident local control plane, Unix socket protocol, and HTTP API
- `crates/cua-capture`: frame bus and capture backend contract
- `crates/cua-input`: input backend contract and typed action results
- `crates/cua-model`: OpenRouter eval harness and model selection report
- `crates/cua-trace`: trace records and validation helpers
- `crates/cua-platform-macos`: macOS backend and permission boundary
- `docs/`: API, config, SDK, eval, and support notes
- `fozzy/`: deterministic scenario verification

## Run

```sh
cargo run -p cua -- serve --addr 127.0.0.1:8765 --hud-mode headful
cargo run -p cua -- status --json
cargo run -p cua -- manifest --json
cargo run -p cua -- metrics --json
cargo run -p cua -- events --json
cargo run -p cua -- ui step "checking target" --task "Click target" --tool vision --step-index 2 --step-total 5 --json
cargo run -p cua -- ui reply "Done with the target." --json
cargo run -p cua -- ui mode headless --json
cargo run -p cua -- ui mode headful --json
cargo run -p cua -- perf bench screenshot --iterations 5 --json
cargo run -p cua -- context --json --force-fresh
cargo run -p cua -- screenshot --out artifacts/cua/smoke/screen.png --json
cargo run -p cua -- profile create smoke --mode supervised --duration-ms 60000 --clipboard --json
cargo run -p cua -- profile activate --json
cargo run -p cua -- clipboard write "hello from cua" --json
cargo run -p cua -- clipboard read --allow-sensitive --json
cargo run -p cua -- pause --json
cargo run -p cua -- resume --json
cargo run -p cua -- kill-switch --json
cargo run -p cua -- config status --json
cargo run -p cua -- stream --unix --frames 3 --json
```

`cua` automatically loads environment values from `CUA_ENV_FILE` and `~/.cua/config/env`. The current directory `.env` is ignored by production runtime code.

`cua serve` refuses non-loopback binds unless `--allow-lan` is explicit. The daemon also opens a profile-local socket at `~/.cua/profiles/<profile>/daemon.sock`; the app runtime uses that socket for context snapshots, visual sessions, and input dispatch. The local APIs use a per-profile bearer token stored at `~/.cua/profiles/<profile>/http.token`; CLI and app commands load it automatically. HTTP write routes also require an owner session id in `x-cua-session-id`. `CUA_HTTP_TOKEN` is a test/development override only when `CUA_DEV_HTTP_TOKEN_OVERRIDE=1` is set. `cua config status --json` reports the canonical `~/.cua` paths and migration state without exposing token contents.

Run `cua serve --hud-mode headful` for the visible dynamic-island HUD or `cua serve --hud-mode headless` for the same resident voice/control event loop without a visible HUD. The HUD process also accepts `cua-voice --headful|--headless` directly. Agents can switch a running HUD with `cua ui mode headless|headful`.

Voice capture and model calls can be tuned with `CUA_VOICE_RECORD_MIN_MS`, `CUA_VOICE_RECORD_SILENCE_MS`, `CUA_VOICE_RECORD_THRESHOLD`, `CUA_VOICE_STT_TIMEOUT_MS`, `CUA_VOICE_STT_RETRY_ATTEMPTS`, `CUA_VOICE_STT_RETRY_BACKOFF_MS`, `CUA_VOICE_PLANNER_TIMEOUT_MS`, `CUA_VOICE_PLANNER_RETRY_ATTEMPTS`, and `CUA_VOICE_PLANNER_RETRY_BACKOFF_MS`. Speech-to-text runs locally by default with Whisper `tiny.en`; pass `--stt-backend openrouter --stt-model openai/gpt-4o-mini-transcribe` to use remote transcription explicitly.

## Package

```sh
scripts/package-macos-app.sh
scripts/install-macos-app.sh
```

The packager builds `cua`, creates `artifacts/cua/macos/cua.app`, signs it with bundle identifier `io.saint0x.cua`, verifies the signature, and prints the app path. If no `CUA_CODESIGN_IDENTITY` is set, it creates and reuses a local `cua Local Developer` code-signing identity so macOS privacy prompts keep a stable app identity across rebuilds. The installer copies that bundle to `~/Applications/cua.app` and registers the stable app path with LaunchServices so macOS privacy prompts show `cua` instead of transient build artifacts.

## Evaluate Models

Create `~/.cua/config/env`:

```sh
OPENROUTER_API_KEY=...
CUA_MODEL_EVAL_LIVE=0
CUA_MODEL_EVAL_MAX_TOKENS=256
CUA_MODEL_EVAL_MAX_CALLS=8
```

Dry run:

```sh
cargo run -p cua -- model eval --max-calls 4 --json
```

Bounded live run:

```sh
cargo run -p cua -- model eval --live --max-calls 8 --json
```

## Verify

```sh
cargo fmt --check
cargo test
cargo check
cargo build -p cua
scripts/host-gui-smoke.sh
scripts/host-voice-smoke.sh
fozzy doctor --deep --scenario fozzy/scenarios/cua-smoke.json --runs 5 --seed 4242 --json
fozzy test --det --strict-verify fozzy/scenarios/cua-smoke.json --json
```

Use `cua perf bench screenshot|stream|input|model-prep --json` for local daemon latency checks. Budgets can be tuned with `CUA_BUDGET_*`, `CUA_STREAM_SOAK_SECONDS`, and `CUA_STREAM_RSS_BUDGET_KB`.

For trace verification:

```sh
CUA_TRACE_DIR=artifacts/cua/macos/trace-smoke cargo run -p cua -- serve --addr 127.0.0.1:8765 &
cargo run -p cua -- --server-addr 127.0.0.1:8765 mouse move 8 8
cargo run -p cua -- trace verify artifacts/cua/macos/trace-smoke --json
cargo run -p cua -- --server-addr 127.0.0.1:8765 trace replay artifacts/cua/macos/trace-smoke --json
mkdir -p artifacts/cua/macos/fozzy
fozzy run fozzy/scenarios/cua-smoke.json --det --record artifacts/cua/macos/fozzy/cua-smoke.fozzy --json
fozzy trace verify artifacts/cua/macos/fozzy/cua-smoke.fozzy --strict --json
fozzy replay artifacts/cua/macos/fozzy/cua-smoke.fozzy --json
```

## Status

The current runtime has production-shaped contracts, daemon/CLI plumbing, Unix socket voice transport, live headless/headful HUD mode switching, persistent Unix visual-control sessions, frame-relative action dispatch, macOS permission probes, profile policy state, pause/resume/kill-switch controls, profile-gated daemon clipboard, daemon-owned capture/encode/input/event/permission/trace/model lanes, ScreenCaptureKit-backed macOS capture with CoreGraphics fallback, native macOS display/cursor/window observation, CGEvent mouse/keyboard input with refusing fallback, signed macOS app packaging, continuous MJPEG/WebSocket streams, schema export, trace inspection, and bounded model evals.

Programmable SDKs are intentionally thin: Rust uses `crates/cua-client`, and TypeScript/Python wrappers live in `sdks/` on top of `cua run` plus the daemon protocol. Attestation and Quilt/cloud enrollment are schema-only in the current source, not shipped CLI/daemon/SDK surfaces. See [docs/sdk.md](docs/sdk.md), [docs/http-api.md](docs/http-api.md), [docs/config-home.md](docs/config-home.md), and [docs/attestation.md](docs/attestation.md).
