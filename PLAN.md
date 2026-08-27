Mission: finish cua for production macOS local use, with no false logic, no dead compatibility paths, and verified end-to-end behavior. Each section below is an independent agent workstream. Agents may work in parallel, but must respect the ownership boundaries and integration contracts here.

Shared coordination rules for every agent:

- Work only from current source, tests, docs, and live command output. Do not assume prior work is complete without evidence.
- Keep `cua-core` as the protocol source of truth. Any public request, response, event, trace, attestation, SDK, or runebook shape belongs there first.
- Prefer Unix socket hot paths for local runtime control. HTTP remains operator/debug unless an agent explicitly hardens it to the same session semantics as Unix.
- Do not add backward compatibility, no-op fallback behavior, mock production paths, or unused branches. If a path is false, remove it.
- Mac only. Do not add Windows/Linux production behavior.
- Keep CLI and HTTP/API exposure for all core functionality, but never let HTTP drift from the canonical protocol.
- Run focused tests for the changed area plus at least one relevant Fozzy deterministic scenario when the work changes runtime behavior.
- When a section is done and verified, delete that section from this file with no replacement prose.
- Before committing, run `cargo fmt --check`, `git diff --check`, and the strongest focused tests that prove the section.

Agent A: Runebook Runtime Owner

Scope:
- Owns `crates/cua-cli/src/main.rs` runebook parsing/execution, `sample-cua-runebook.md`, runebook fixtures under `tests/fixtures/`, and runebook Fozzy scenarios.
- Turns runebooks into the canonical compact scripting surface over the daemon protocol.

Must coordinate with:
- Agent B for attestation runebook nodes.
- Agent C for visual/watch-session nodes.
- Agent D for model/STT/planner turn nodes.
- Agent E for session/transport rules.
- Agent H for docs/examples once behavior is verified.

Work:
- Support the remaining top-level runebook shape from `sample-cua-runebook.md`: `[daemon]`, `[attest]`, `[stt.<name>]`, `[planner.<name>]`, and `[memory]`.
- Finish raw `rpc` escape hatch for HTTP protocol coverage.
- Support `$result.path` references and result-based interpolation beyond simple saved scalars.
- Add remaining direct protocol steps: doctor, permissions status/preflight, visual session, observe displays/cursor, trace start/inspect/verify/replay, perf bench, model eval, and schema export.
- Add workflow nodes: `parallel`, `race`, `batch`, `foreach`, `run`, and `spawn_run`.
- Add control-flow/timing nodes: `if`, `if_present`, result-based branching, sleep, timers, delayed messages, and first-class waits.
- Implement global and step-level `on_error = "ask" | "rollback"`.
- `ask` must pause execution and request an operator decision through HUD/CLI.
- `rollback` must run explicit rollback steps when present; otherwise stop with a clear unsupported rollback error.
- Trace every step start/complete/error, error-policy decision, rollback action, child runebook, and replay-relevant output.
- Add runebook-level trace verify/replay and shrinking.

Done means:
- `sample-cua-runebook.md` no longer describes unimplemented runebook constructs.
- Every runebook node listed above has a direct passing fixture or unit test.
- At least one deterministic Fozzy scenario records, verifies, and replays a runebook trace.
- The runebook executor has no "not implemented yet" branches for documented runebook features.

Agent B: Identity, Attestation, And Enrollment Owner

Scope:
- Owns local machine identity, signed runtime attestation, enrollment metadata, attestation CLI/HTTP/Unix methods, attestation schemas, and attestation docs.
- Primary files: `crates/cua-core/src/lib.rs`, `crates/cua-daemon/src/lib.rs`, `crates/cua-cli/src/main.rs`, `tests/fixtures/schema-bundle.json`, `docs/attestation.md`.

Must coordinate with:
- Agent A for `[attest]` and `do = "attest"` runebook support.
- Agent E for profile/session semantics in attestation claims.
- Agent F for canonical `~/.cua/identity` and `~/.cua/cloud` paths.
- Agent H for docs and examples.

Work:
- Finish local machine identity storage under `~/.cua/identity/`.
- Store non-secret metadata in `~/.cua/identity/machine.json`.
- Implement the resolved private-key backend policy. Prefer Secure Enclave-backed keys when practical; otherwise use macOS Keychain. File keys may exist only for explicit test/dev mode.
- Record only public key and key metadata in normal files.
- Support key rotation: current key, previous keys, creation time, and revocation marker.
- Provide `cua identity status --json` and `cua identity rotate --json`.
- Add host-backed tests proving key persistence across daemon restarts.
- Add daemon HTTP endpoints: `POST /attestation/challenge`, `POST /attestation/sign`, and `GET /attestation/identity`.
- Add Unix RPC methods: `attestation.challenge`, `attestation.sign`, and `attestation.identity`.
- Add CLI commands: `cua attestation challenge --audience <audience> --json`, `cua attestation sign --audience <audience> --nonce <nonce> --json`, `cua attestation identity --json`, and `cua attestation verify <attestation.json> --json`.
- Include nonce, audience, issued/expires timestamps, profile, session id when present, daemon pid, runtime version, socket path, HTTP addr, active profile/capabilities, safety state, permission report, code signing identity where available, binary hash where available, machine public key id, and salted machine id hash.
- Refuse expired challenges, audience mismatch, unsigned/unknown keys, stale timestamps, and reused challenge ids where practical.
- Reuse facts from `scripts/host-package-proof.sh`: app path, bundle id, executable, code signature, designated requirement, daemon binary signature, voice binary signature, ctx binary signature, and usage descriptions.
- Add `CodeIdentityClaims` and `runtime_integrity_level = packaged_signed | development_signed | development_unsigned | unknown`.
- Add cloud enrollment: `cua enroll --audience quilt-cloud --json`, `cua enroll status --json`, local storage at `~/.cua/cloud/enrollments/<audience>.json`, multiple audiences, revocation handling, and SDK `client.attest({ audience, nonce })`.
- Add Quilt-side verifier design: nonce, signature, public key enrollment, code identity policy, profile/capability policy, and timestamp freshness.

Done means:
- Identity and attestation work through CLI, Unix socket, and HTTP.
- Verification rejects bad audience, expired challenge, reused challenge id, altered payload, unknown key, and stale timestamp.
- Packaged and development integrity claims are present and documented.
- Host proof covers key persistence and signed package attestation claims.
- `docs/attestation.md` explains threat model, what attestation proves, and what it does not authorize.

Agent C: Visual Stream, Watch Session, And Low-Latency Observation Owner

Scope:
- Owns visual sessions, persistent screen streams, watch-session lifecycle, backpressure/cancellation, and model-facing visual sampling.
- Primary files: `crates/cua-client`, `crates/cua-daemon`, `crates/cua-cli`, SDK visual streaming helpers, host visual proof scripts.

Must coordinate with:
- Agent D for feeding sampled frames into planner/model loops.
- Agent E for observer-session semantics.
- Agent G for performance/latency proof.
- Agent H for visual session examples.

Work:
- Expose model-facing watch sessions that can be opened, closed, or bounded by duration.
- Keep Unix `visual.session` as the hot transport path.
- Add explicit cancellation, backpressure, and bounded frame queues.
- Support visual session streaming in TypeScript and Python SDKs.
- Feed sampled visual frames into planner loops without blocking low-latency action dispatch.
- Verify final actions against fresh stream/screenshot evidence before completion.
- Add trace/Fozzy coverage for visual stream open, frame receive, action dispatch, verification screenshot, and close.

Done means:
- A headless agent can keep a persistent stream open while dispatching actions on the same profile.
- Stream close/cancel is reliable and does not leak sessions or daemon tasks.
- SDK examples can consume frames and dispatch frame-relative actions.
- Host visual-session action proof passes.

Agent E: Transport, Session, Permissions, And SDK Semantics Owner

Scope:
- Owns Unix/HTTP transport policy, owner/observer sessions, SDK mutation semantics, permission request behavior, and typed client errors.
- Primary files: `crates/cua-client`, `crates/cua-daemon`, `crates/cua-cli`, `sdks/typescript`, `sdks/python`, `docs/http-api.md`.

Must coordinate with:
- Agent B for attestation session claims.
- Agent C for observer visual sessions.
- Agent H for SDK docs/examples.

Work:
- Finish `cua-client` as a reusable Rust SDK crate with typed errors instead of generic `anyhow` strings for protocol errors.
- Preserve idempotency support for input requests. SDK callers must not accidentally create untraceable duplicate actions.
- Prefer Unix socket transport by default.
- Keep HTTP optional for operator/debug and enforce owner-session ids on HTTP write routes.
- Use `session.acquire` for every SDK code path that mutates state.
- Require owner sessions for writes: input dispatch, frame input dispatch, profile create/activate, pause/resume/kill, and clipboard write.
- Allow observer sessions for reads: status, manifest, schemas, screenshot, context, observe, events, and visual session.
- Keep HTTP write-route policy aligned with the implemented `x-cua-session-id` owner-session requirement.
- Keep `CUA_HTTP_TOKEN` as a dev/test-only override gated by `CUA_DEV_HTTP_TOKEN_OVERRIDE=1`.
- Add owner-session heartbeat/lease renewal, lease expiry tests, and explicit refusal evidence for writes attempted without an owner lease.
- Finish advanced TypeScript/Python SDK parity: local Unix where supported, HTTP fallback where practical, generated/checked-in protocol types, attestation helpers, event streaming, visual streaming, trace verify/replay helpers, and model eval helper.

Done means:
- SDK default mode is read-only until explicit owner acquisition.
- Unauthorized writes fail consistently on all supported transports.
- Lease expiry and heartbeat behavior are tested.
- TypeScript, Python, Rust client APIs align on names and semantics.

Agent F: Config Home, ctx, Chat DB, Memory, And Scratchpad Owner

Scope:
- Owns all durable path policy under `~/.cua`, ctx binary resolution, chat persistence, ctx memory integration, scratchpads, env loading policy, and migration.
- Primary files: `crates/cua-core`, `crates/cua-platform-macos`, `crates/cua-cli`, `frontends/cua-voice`, release/package scripts, `docs/config-home.md`.

Must coordinate with:
- Agent B for identity/cloud paths.
- Agent D for planner memory injection.
- Agent E for profile/session files.
- Agent H for docs.

Work:
- Normalize durable runtime/config state under `~/.cua/{concern}/**/*`.
- Preserve valid existing paths for profile chat DB, ctx workspace, HTTP token, and daemon socket.
- Migrate `~/.cua/.env` to `~/.cua/config/env`; current-directory `.env` is not loaded by runtime code.
- Make scripts use `CUA_HOME` and concern-specific artifact paths.
- Ensure app runtime logs/proofs use `~/.cua/artifacts/...` where appropriate.
- Update `ctx_binary()` so production prefers packaged sibling or `~/.cua/bin/ctx`, not repo `vendor/ctx/ctx` except explicit dev mode.
- Keep ctx resolution on packaged sibling first, then `~/.cua/bin/ctx`, with repo-local ctx only behind explicit development mode.
- Implement local chat DB persistence and automatic feed back into the agent loop.
- Use the vendored `ctx` binary as the required memory layer, not fallback logic.
- Add agent-accessible ctx tool and automatic chat/context feeding.
- Add ephemeral and durable scratchpads under `~/.cua/profiles/<profile>/scratchpads/`.
- Update runtime prompt/instructions so useful discoveries, decisions, environment facts, and project context are appended selectively.
- Append and retrieve scratchpad memory in parallel with other runtime work whenever possible.

Done means:
- Installed app runtime does not depend on repo paths.
- `CUA_HOME` tests prove shared path helpers and config migration.
- ctx is resolved from the production path or explicit dev override.
- Chat DB, ctx frame retrieval, and scratchpad append/read are verified in the voice/planner path.

Agent G: Validation, Fozzy, Release, And Performance Owner

Scope:
- Owns production proof commands, Fozzy scenarios, host proof scripts, release script gates, latency/perf benchmarks, and trace artifacts.
- Primary files: `fozzy/scenarios`, `scripts/host-*.sh`, `scripts/release.sh`, test fixtures, artifacts policy.

Must coordinate with:
- Every agent for the proof command matching their feature.
- Agent H for documenting the final release gate.

Work:
- Add unit tests for shared path helpers with `CUA_HOME`.
- Add unit tests for env loading precedence.
- Add challenge expiry and audience mismatch tests.
- Add host proof for machine key persistence.
- Add host proof for signed package attestation claims.
- Add host proof for SDK owner-session mutation.
- Add host proof for SDK observer read-only behavior.
- Add host proof for config migration.
- Add Fozzy deterministic scenario covering SDK status/context/dispatch.
- Add Fozzy trace recording and replay for SDK action path.
- Maintain required shipping command set:

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

Done means:
- Every shipping command passes or has a documented, fixed reason replaced by a stronger equivalent proof.
- Fozzy has strict deterministic coverage for runtime smoke, runebook trace replay, and SDK action path.
- Release script fails early on missing dependencies, permissions, stale schemas, broken host proofs, or uncommitted generated artifacts.

Agent H: Documentation, Examples, And Agent Surface Owner

Scope:
- Owns user-facing docs, architecture docs, SDK examples, `cua.md`, README, and prompt/tool surface inventory.
- Primary files: `README.md`, `cua.md`, `docs/*.md`, SDK READMEs/examples.

Must coordinate with:
- Every implementation agent. Do not document a feature as complete until the owning agent provides proof.

Work:
- Add `docs/attestation.md`.
- Add `docs/config-home.md`.
- Update `docs/http-api.md`.
- Update `cua.md`.
- Update `README.md`.
- Keep `cua.md` listing all agent tools and system prompts accurately.
- Add SDK examples:
  - minimal local status check
  - observe screenshot/context
  - click using frame-relative coordinates
  - acquire owner, run a sequence, release owner
  - visual session and process frames
  - show UI progress in the HUD
  - read/write clipboard with explicit capability profile
  - request attestation and verify it locally
  - enroll with Quilt/cloud
  - use Aegis through cua
  - use ctx through cua
- Ensure docs explain profile policy vs bearer token vs owner session vs attestation.

Done means:
- Docs match current shipped behavior and do not mention unsupported features as available.
- Every example is either smoke-tested or directly backed by a passing fixture/proof.
- README is concise, feature-focused, and production-oriented.
