# cua Remaining Production Plan

This plan tracks the remaining work to turn cua into a lightweight, programmable local-computer SDK with machine attestation and a clean config-home layout.

The north star:

> cua should expose a trusted local computer-use control plane that agents can script programmatically, while proving which local machine/runtime/profile they are controlling.

## 0. Implement cua runebooks as the canonical scripting surface

The runebook is the proposed compact, structured `cua run <file>.toml` execution format. It should become the first-class scripting surface before public TypeScript/Python SDKs.

Reference design:

- `sample-cua-runebook.md`

Required work:

1. Add `cua run <file>` to `crates/cua-cli`.
2. Add a typed runebook parser and validator, either in a new `crates/cua-runebook` crate or in `cua-core` if the types should be part of the stable protocol schema.
3. Support the full top-level runebook shape from `sample-cua-runebook.md`:
   - `schema`
   - `[run]`
   - `[daemon]`
   - `[session]`
   - `[attest]`
   - `[stt.<name>]`
   - `[planner.<name>]`
   - `[memory]`
   - `[trace]`
   - `[vars]`
   - `[macro.<name>]`
   - `[[steps]]`
4. Compile compact aliases to the existing cua protocol:
   - `click`
   - `drag`
   - `key`
   - `type`
   - `paste`
   - `open_app`
   - `shell`
   - `aegis`
   - `ctx`
5. Support canonical raw input through existing `InputAction` shapes.
6. Support `input.frame` by compiling to frame-relative input dispatch.
7. Support the raw `rpc` escape hatch for full Unix/HTTP protocol coverage.
8. Support `save_as` result binding.
9. Support `${var}` interpolation and `$result.path` references.
10. Support direct protocol steps:
   - status, doctor, manifest, schemas, metrics
   - permissions
   - sessions
   - profiles
   - context, screenshot, window capture, visual session
   - observe desktop/displays/cursor
   - events and waits
   - UI step/reply/mode/island
   - clipboard read/write
   - pause/resume/kill switch
   - trace start/inspect/verify/replay
   - perf bench
   - model eval
   - schema export
11. Support workflow nodes:
   - `seq`
   - `parallel`
   - `race`
   - `batch`
   - `foreach`
   - `run`
   - `spawn_run`
12. Support built-in model/voice turn nodes:
   - text turn
   - WAV turn
   - live record turn
   - STT-only step
   - planner-only step
   - generic model call step
   - dispatch model output as cua action
   - spawn child runebook from model output
13. Add a runebook trace format and write a trace for every runebook execution.
14. Add trace verify/replay support for runebook traces.
15. Add deterministic fozzy scenarios for runebook parsing, validation, execution, trace verification, and replay.
16. Add docs that make clear the runebook format is the canonical programmable surface and SDKs are convenience layers over the same protocol/runebook substrate.

Current source reality and deduped implementation gaps:

1. The runebook runtime is not implemented:
   - no `cua run <file>` command
   - no runebook parser, validator, executor, schema, or trace format
   - no `[[steps]]` execution model
   - no variable interpolation, `save_as`, result references, macros, or compact alias compiler
2. Global and step-level error policy is not implemented:
   - `on_error = "stop" | "continue" | "ask" | "rollback"` is not implemented
   - existing source has localized retry/error behavior for STT, planner, capture, trace verification, and normal command failures, but no global runebook error policy
   - `ask` needs an operator decision path through CLI/HUD
   - `rollback` needs explicit rollback handlers and trace semantics
3. Workflow composition is not implemented as runebook functionality:
   - `seq`, `parallel`, `race`, `batch`, `foreach`, `run`, and `spawn_run`
   - `InputAction::Sequence` exists for batched input actions, but it is not a general workflow engine
4. Attestation and enrollment runebook fields are not implemented:
   - `[attest]`
   - `do = "attest"`
   - machine identity status
   - enrollment
   - signed local-machine proofs
   - cloud trust metadata
5. Generic model steps are not implemented:
   - `do = "model"`
   - `dispatch_model_action`
   - model output spawning a child runebook
   - current source only has voice planner internals and `model eval`
6. Voice/STT/planner turn steps are only partially backed:
   - text, WAV, and live-record turns exist in `frontends/cua-voice`
   - STT and planner config exists through voice CLI flags and env vars
   - `[stt.<name>]`, `[planner.<name>]`, `[memory]`, `do = "turn"`, STT-only steps, planner-only steps, and scheduled/programmatic model messages are not exposed through a runebook executor
7. Runebook trace verification and replay are not implemented:
   - existing trace start/inspect/verify/replay is action-trace oriented
   - runebook-level traces, step-result traces, error-policy traces, verify-on-complete, replay, and shrinking need separate implementation
8. Conditions and timing are not implemented:
   - `if`, `if_present`, result-based branching, sleeps, timers, delayed messages, and first-class waits are proposed only
9. Config-home normalization is not complete:
   - durable state is partly under `~/.cua/profiles/<profile>/...`
   - scripts, proof artifacts, docs, and some tool discovery paths still need to consistently use the canonical `~/.cua/{concern}/**/*` layout
10. HTTP write-session parity is not complete:
   - Unix RPC write paths enforce owner-session checks
   - HTTP write routes appear bearer-token-only today
   - runebooks and SDKs should prefer Unix until HTTP has equivalent session semantics
11. Several direct protocol operations are backed by source but still need runebook adapters:
   - status, manifest, schemas, metrics, health
   - screenshot, window capture, context, observe, events
   - visual sessions
   - UI step/reply/mode/island
   - profiles and sessions
   - pause/resume/kill switch
   - clipboard
   - shell, aegis, ctx, open app, mouse, keyboard
   - model eval, schema export, trace start/inspect/verify/replay

Required `on_error` implementation:

1. `stop`: stop at the first failed step, record the error in the run trace, return non-zero.
2. `continue`: record the error and continue to the next eligible step.
3. `ask`: pause execution and request an operator decision through the HUD/CLI before proceeding.
4. `rollback`: run explicit rollback steps when present; otherwise stop with a clear unsupported rollback error.
5. Allow step-level `on_error` to override `[run].on_error`.
6. Include error policy decisions in the runebook trace.

## 1. Extract a reusable `cua-client` SDK crate

The voice frontend already contains a real client abstraction in `frontends/cua-voice/src/client.rs`. It should move into a reusable workspace crate instead of living inside the HUD/voice frontend.

Required work:

1. Add `crates/cua-client` as a workspace member.
2. Move the Unix socket client from `frontends/cua-voice/src/client.rs` into `crates/cua-client`.
3. Make `frontends/cua-voice` depend on `cua-client` instead of owning its own copy.
4. Make `crates/cua-cli` use `cua-client` for Unix socket calls instead of duplicating token/path/request logic in `crates/cua-cli/src/main.rs`.
5. Keep `cua-core` as the protocol source of truth for request/response types.
6. Add a small stable public API:
   - `CuaClient::connect(profile)`
   - `CuaClient::status()`
   - `CuaClient::manifest()`
   - `CuaClient::schemas()`
   - `CuaClient::context(...)`
   - `CuaClient::screenshot(...)`
   - `CuaClient::observe()`
   - `CuaClient::events()`
   - `CuaClient::events_after(sequence)`
   - `CuaClient::events_wait(sequence, timeout_ms)`
   - `CuaClient::acquire_owner(client_name, ttl_ms)`
   - `CuaClient::acquire_observer(client_name, ttl_ms)`
   - `CuaClient::dispatch(action)`
   - `CuaClient::dispatch_frame(source_frame, action)`
   - `CuaClient::visual_session(...)`
   - `CuaClient::pause()`
   - `CuaClient::resume()`
   - `CuaClient::kill_switch()`
   - `CuaClient::ui_step(...)`
   - `CuaClient::ui_reply(...)`
   - `CuaClient::ui_mode(...)`
   - `CuaClient::clipboard_read(...)`
   - `CuaClient::clipboard_write(...)`
7. Preserve idempotency support for input requests. Do not let SDK callers accidentally create untraceable duplicate actions.
8. Add typed error handling instead of returning generic `anyhow` strings for protocol errors.

Why this matters:

- The SDK should be a thin, reliable client over the existing daemon protocol, not a second runtime.
- The CLI and voice frontend should become consumers of the same SDK users get.
- The SDK crate becomes the source for TypeScript/Python SDKs later.

## 2. Prefer Unix socket transport by default

The current runtime exposes CLI, profile-local Unix socket, and loopback HTTP. The SDK should prefer Unix socket transport for local scripting.

Required work:

1. Make Unix socket the default `cua-client` transport.
2. Keep HTTP as an optional operator/debug transport.
3. Use the profile-local socket path by default:
   - `~/.cua/profiles/<profile>/daemon.sock`
4. Load the profile bearer token by default from:
   - `~/.cua/profiles/<profile>/http.token`
5. Allow explicit token override through `CUA_HTTP_TOKEN` only as a development/CI override.
6. Use `session.acquire` for any SDK code path that mutates state.
7. Require an owner session for writes:
   - input dispatch
   - frame input dispatch
   - profile create/activate
   - pause/resume/kill
   - clipboard write
8. Allow observer sessions for reads:
   - status
   - manifest
   - schemas
   - screenshot
   - context
   - observe
   - events
   - visual session

Current source reality:

- Unix write paths already enforce session owner checks in `crates/cua-daemon/src/lib.rs`.
- HTTP routes currently appear bearer-token-only and do not have the same owner-session write gate.
- SDK mutation should therefore prefer Unix until HTTP has equivalent session semantics.

## 3. Add first-class machine attestation types to `cua-core`

Machine attestation should be part of the protocol, not an external script.

Required `cua-core` types:

1. `AttestationChallengeRequest`
   - `schema_version`
   - `audience`
   - optional `profile`
   - optional requested claims
2. `AttestationChallenge`
   - `schema_version`
   - `challenge_id`
   - `nonce`
   - `audience`
   - `issued_wall_ms`
   - `expires_wall_ms`
3. `MachineIdentity`
   - `schema_version`
   - `machine_key_id`
   - `machine_public_key`
   - `machine_id_hash`
   - `created_wall_ms`
   - `key_backend`
4. `RuntimeIdentityClaims`
   - `schema_version`
   - `runtime_name`
   - `runtime_version`
   - `daemon_pid`
   - `profile`
   - `socket_path`
   - `http_addr`
   - `bundle_id`
   - `designated_requirement`
   - `code_signature_summary`
   - `binary_sha256`
   - `permissions`
   - `active_profile`
   - `safety_state`
   - `session_id`
5. `MachineAttestation`
   - `schema_version`
   - `challenge`
   - `identity`
   - `claims`
   - `signature_algorithm`
   - `signature`
   - `signed_wall_ms`
6. Add all attestation types to `schema_bundle()`.
7. Update `tests/fixtures/schema-bundle.json`.

Design rule:

- Do not expose raw hardware serials or platform UUIDs. Use a tenant/project/audience-salted machine hash.

## 4. Implement local machine identity storage

The daemon needs a durable machine key so it can sign attestation challenges.

Required work:

1. Create a machine identity concern under `~/.cua/identity/`.
2. Store non-secret metadata in:
   - `~/.cua/identity/machine.json`
3. Store the private key in macOS Keychain when possible.
4. Prefer Secure Enclave-backed keys when practical.
5. If Secure Enclave is not available, use a Keychain-protected signing key.
6. Record only public key and key metadata in files.
7. Add key rotation support:
   - current key
   - previous keys
   - creation time
   - revocation marker
8. Add `cua identity status --json`.
9. Add `cua identity rotate --json`.
10. Add host-backed tests that prove the key persists across daemon restarts.

Suggested config paths:

- `~/.cua/identity/machine.json`
- `~/.cua/identity/keys/current.json`
- `~/.cua/identity/keys/previous/<key_id>.json`

## 5. Add attestation protocol methods to the daemon

Required HTTP endpoints:

1. `POST /attestation/challenge`
2. `POST /attestation/sign`
3. `GET /attestation/identity`

Required Unix socket RPC methods:

1. `attestation.challenge`
2. `attestation.sign`
3. `attestation.identity`

Required CLI commands:

1. `cua attestation challenge --audience <audience> --json`
2. `cua attestation sign --audience <audience> --nonce <nonce> --json`
3. `cua attestation identity --json`
4. `cua attestation verify <attestation.json> --json`

Attestation document must include:

- nonce
- audience
- issued/expires timestamps
- profile
- session id when present
- daemon pid
- runtime version
- socket path
- HTTP addr
- active profile/capabilities
- safety state
- permission report
- code signing identity where available
- binary hash where available
- machine public key id
- salted machine id hash

Security rules:

- Refuse expired challenges.
- Refuse audience mismatch.
- Refuse unsigned or unknown machine keys.
- Include freshness checks.
- Keep challenge nonces single-use where practical.
- Do not treat attestation as authorization by itself. It proves machine/runtime identity; profile/session policy still controls action authority.

## 6. Use existing package proof as attestation evidence

`scripts/host-package-proof.sh` already proves important macOS packaging facts:

- app path
- bundle id
- executable
- code signature
- designated requirement
- daemon binary signature
- voice binary signature
- ctx binary signature
- usage descriptions

Required work:

1. Move the reusable proof logic into Rust or a small shared script that the daemon can call safely.
2. Add a `CodeIdentityClaims` type to `cua-core`.
3. Include package/code identity claims in attestation when running from the packaged app.
4. Include a lower-confidence development identity when running from `target/debug`.
5. Add a clear `runtime_integrity_level` field:
   - `packaged_signed`
   - `development_signed`
   - `development_unsigned`
   - `unknown`
6. Make verification policies able to require `packaged_signed` for cloud enrollment.

## 7. Add cloud enrollment and verification flow

This connects local cua to Quilt/cloud.

Required work:

1. Add `cua enroll --audience quilt-cloud --json`.
2. Enrollment should:
   - generate or load machine identity
   - request a cloud challenge
   - sign the challenge locally
   - submit public key and attestation
   - receive an enrollment id
   - store enrollment metadata under `~/.cua/cloud/`
3. Store cloud enrollment data at:
   - `~/.cua/cloud/enrollments/<audience>.json`
4. Support multiple audiences/tenants without overwriting identity.
5. Add revocation handling.
6. Add `cua enroll status --json`.
7. Add SDK method:
   - `client.attest({ audience, nonce })`
8. Add Quilt-side verifier design:
   - verify nonce
   - verify signature
   - verify public key enrollment
   - verify code identity policy
   - verify profile/capability policy
   - verify timestamp freshness

## 8. Define the public scripting SDKs

After `crates/cua-client` is stable, build public language SDKs.

Required TypeScript SDK:

1. Package name: decide between `@quilt/cua`, `@cua/sdk`, or `cua-sdk`.
2. Connect to local Unix socket on macOS where supported.
3. Fall back to loopback HTTP where Unix socket is not practical.
4. Generate types from `/schemas` or checked-in schema bundle.
5. Support visual streaming.
   - expose model-facing watch sessions that can be opened, closed, or bounded by duration
   - feed sampled visual frames into the planner loop without blocking low-latency action dispatch
   - keep the Unix visual session as the hot transport path and add explicit cancellation/backpressure
   - verify final actions against fresh stream/screenshot evidence before completion
6. Support owner/observer session semantics.
7. Support attestation.
8. Support frame-relative input dispatch.
9. Support event long-polling/streaming.

Required Python SDK:

1. Package name: `cua-sdk`.
2. Same transport and session semantics as TypeScript.
3. Include examples for local agent scripts and notebook use.

Example target API:

```ts
const cua = await Cua.connect({ profile: "default" });
const owner = await cua.acquireOwner({ clientName: "agent" });
const context = await cua.context({ maxWidth: 1280, includeBytes: true });

await cua.dispatchFrame({
  session: owner,
  sourceFrame: context.frame.envelope,
  action: { kind: "mouse_click", x: 420, y: 240, button: "left", count: 1 },
});
```

## 9. Normalize all config under `~/.cua/{concern}/**/*`

All durable cua runtime/config state should live under `~/.cua` by concern. No production path should be assembled ad hoc in each crate.

Target layout:

```text
~/.cua/
  config/
    env
    settings.json
  identity/
    machine.json
    keys/
      current.json
      previous/
  profiles/
    <profile>/
      profile.json
      http.token
      daemon.sock
      chat.db
      ctx/
      traces/
        voice.jsonl
        daemon/
      screenshots/
      downloads/
      uploads/
      sessions/
      scratchpads/
        ephemeral/
        durable/
  cloud/
    enrollments/
      <audience>.json
  logs/
    daemon/
    voice/
  cache/
    frames/
    model/
  artifacts/
    proofs/
    release/
    fozzy/
  bin/
    ctx
```

Required work:

1. Use profile-local environment loading only.
2. Add migration from old paths to new paths:
   - `~/.cua/.env` -> `~/.cua/config/env`
   - `~/.cua/profiles/<profile>/chat.db` stays valid
   - `~/.cua/profiles/<profile>/ctx` stays valid
   - `~/.cua/profiles/<profile>/http.token` stays valid
   - `~/.cua/profiles/<profile>/daemon.sock` stays valid
3. Make scripts use `CUA_HOME` and concern-specific artifact paths.
4. Add first-class agent-authored scratchpad docs:
   - support ephemeral scratchpads for short-lived run/session reasoning
   - support durable scratchpads for longer-standing project/profile notes
   - keep the primitive intentionally lightweight for now: files the agent can write, read, and reference
   - update the agent system prompt/runtime instructions so the agent appends useful discoveries, decisions, environment facts, and project context that may help future work
   - keep memory appends selective and work-relevant so scratchpads stay useful instead of becoming noisy transcripts
   - append and retrieve scratchpad memory in parallel with other runtime work whenever possible, so memory hygiene does not add avoidable turn latency
   - make blocking memory reads explicit only when the next action truly depends on that memory
5. Keep scratchpads profile-scoped by default, with a path shape like:
   - `~/.cua/profiles/<profile>/scratchpads/ephemeral/`
   - `~/.cua/profiles/<profile>/scratchpads/durable/`

## 10. Current config/path gaps to fix

These are the concrete gaps found in the current source scan.

1. `crates/cua-cli/src/main.rs`
   - Decide whether current directory `.env` should remain development-only.
2. `crates/cua-platform-macos/src/lib.rs`
   - `aegis_binary()` searches `~/.local/bin/aegis`, Homebrew, and `/usr/local/bin`.
   - This is fine for external tool discovery, but any cua-owned tool should resolve under `~/.cua/bin/` or packaged sibling first.
   - `ctx_binary()` falls back to `vendor/ctx/ctx`; production should prefer packaged sibling or `~/.cua/bin/ctx`.
3. `scripts/package-macos-app.sh`
   - Defaults package output to repo-local `artifacts/cua/macos`.
   - Build output can remain repo-local, but installed/runtime config must not depend on repo paths.
   - Consider copying bundled tools into `~/.cua/bin/` or documenting packaged sibling resolution.
4. `scripts/release.sh`
   - Defaults install target to `$HOME/Applications`.
   - Defaults release artifacts to repo-local `artifacts/cua/release/<run_id>`.
   - Release artifacts can stay repo-local, but runtime logs/proofs generated by the app should use `~/.cua/artifacts/...`.
5. Host proof scripts
   - Many scripts default outputs to `artifacts/cua/...`.
   - Keep repo-local outputs for development tests, but add `CUA_HOME` support and/or mirror final proof artifacts under `~/.cua/artifacts/proofs/...` for installed runtime flows.
6. Documentation
   - `docs/http-api.md` and `cua.md` should document the canonical `~/.cua/{concern}/**/*` layout.

## 11. Make config discoverable through the protocol

The SDK should not guess local paths.

Required work:

1. Add config path information to `RuntimeInventory` or a new `ConfigInventory` type:
   - `cua_home`
   - `profile_root`
   - `profile_socket`
   - `profile_token_present`
   - `chat_db`
   - `ctx_workspace`
   - `trace_root`
   - `identity_root`
   - `cloud_root`
2. Add:
   - `GET /config/status`
   - Unix `config.status`
   - `cua config status --json`
3. Redact secrets and tokens.
4. Include migration status:
   - old env path present
   - new env path present
   - migrated
   - conflicts

## 12. Harden profile/session semantics for SDK users

Required work:

1. Make owner-session write enforcement consistent across transports.
2. Add HTTP session id support for write routes, or mark HTTP writes as development/operator-only.
3. Add SDK defaults:
   - read-only observer by default
   - explicit `acquireOwner()` before mutations
4. Add owner-session heartbeat/lease renewal.
5. Add lease expiry tests.
6. Add explicit refusal evidence for writes attempted without owner lease.
7. Add docs explaining profile policy vs bearer token vs owner session:
   - bearer token authenticates local profile access
   - owner session authorizes mutation
   - profile policy controls capabilities
   - attestation proves runtime/machine identity

## 13. Make all functionality scriptable

The SDK should cover the whole documented agent surface from `cua.md`.

Required SDK coverage:

1. Status, manifest, schemas, metrics.
2. Screenshot and window capture.
3. Context snapshot.
4. Desktop, display, cursor, and window observation.
5. Event snapshot, after, and wait.
6. Visual session streaming.
7. Accessibility permission request.
8. Session acquire/cancel/status.
9. UI step, reply, mode, island.
10. Profile create/activate/status.
11. Pause/resume/kill switch.
12. Input dispatch:
    - mouse move
    - mouse click
    - mouse drag
    - key press
    - key type
    - key paste
    - sequence
    - open app
    - shell exec
    - Aegis
    - ctx
13. Frame-relative input dispatch.
14. Clipboard read/write through explicit clipboard endpoints.
15. Model eval only as an optional advanced surface.
16. Trace verify/replay helpers.
17. Attestation identity/challenge/sign/verify.

## 14. Add docs and examples for the SDK

Required docs:

1. `docs/sdk.md`
2. `docs/attestation.md`
3. `docs/config-home.md`
4. Update `docs/http-api.md`.
5. Update `cua.md`.
6. Update `README.md`.

Required examples:

1. Minimal local status check.
2. Observe screenshot/context.
3. Click using frame-relative coordinates.
4. Acquire owner, run a sequence, release owner.
5. Start a visual session and process frames.
6. Show UI progress in the HUD.
7. Read/write clipboard with explicit capability profile.
8. Request attestation and verify it locally.
9. Enroll with Quilt/cloud.
10. Use Aegis through cua.
11. Use ctx through cua.

## 15. Validation and release checks

Required tests:

1. Unit tests for shared path helpers with `CUA_HOME`.
2. Unit tests for env loading precedence.
3. Unit tests for attestation schema serialization.
4. Unit tests for challenge expiry and audience mismatch.
5. Unit tests for machine id hashing without raw hardware ids.
6. Host proof for machine key persistence.
7. Host proof for signed package attestation claims.
8. Host proof for SDK owner-session mutation.
9. Host proof for SDK observer read-only behavior.
10. Host proof for config migration.
11. Fozzy deterministic scenario covering SDK status/context/dispatch.
12. Fozzy trace recording and replay for SDK action path.

Required commands before shipping:

```sh
cargo fmt --check
cargo test
cargo check
cargo build -p cua
scripts/host-session-proof.sh
scripts/host-control-surface-proof.sh
scripts/host-visual-session-action-proof.sh
scripts/host-package-proof.sh
fozzy doctor --deep --scenario fozzy/scenarios/cua-smoke.json --runs 5 --seed 4242 --json
fozzy test --det --strict fozzy/scenarios/cua-smoke.json --json
fozzy run fozzy/scenarios/cua-smoke.json --det --record ~/.cua/artifacts/fozzy/cua-smoke.fozzy --json
fozzy trace verify ~/.cua/artifacts/fozzy/cua-smoke.fozzy --strict --json
fozzy replay ~/.cua/artifacts/fozzy/cua-smoke.fozzy --json
```

## 16. Open decisions

1. SDK package naming:
   - `@quilt/cua`
   - `@cua/sdk`
   - `cua-sdk`
2. Attestation backend:
   - Secure Enclave first
   - Keychain first
   - file key only for tests
3. Whether HTTP write routes should remain supported after SDK launch.
4. Whether `CUA_HTTP_TOKEN` should remain a production override or become dev/test only.
5. Whether current-directory `.env` loading should be removed entirely or kept as dev-only behavior.
6. Whether `ctx` should be installed into `~/.cua/bin/ctx` or only resolved as a packaged sibling.
7. Whether cloud enrollment belongs in this repo or in Quilt cloud with a local cua client.
