1. Implement cua runebooks as the canonical scripting surface.
   - Use `sample-cua-runebook.md` as the reference design.
   - Support the remaining top-level runebook shape from `sample-cua-runebook.md`: `[daemon]`, `[attest]`, `[stt.<name>]`, `[planner.<name>]`, and `[memory]`.
   - Support the raw `rpc` escape hatch for full Unix/HTTP protocol coverage.
   - Support `$result.path` references.
   - Support remaining direct protocol steps: doctor, schemas, permissions, session acquire/cancel, profile create/activate, screenshot, window capture, visual session, observe displays/cursor, event waits, UI island, trace start/inspect/verify/replay, perf bench, model eval, and schema export.
   - Support workflow nodes: `parallel`, `race`, `batch`, `foreach`, `run`, and `spawn_run`.
   - Support built-in model/voice turn nodes: text turn, WAV turn, live record turn, STT-only step, planner-only step, generic model call step, dispatch model output as cua action, and spawn child runebook from model output.
   - Add trace verify/replay support for runebook traces.
   - Add deterministic fozzy scenarios for remaining runebook trace verification and replay.
   - Add docs that make clear the runebook format is the canonical programmable surface and SDKs are convenience layers over the same protocol/runebook substrate.

2. Implement global and step-level runebook error policy.
   - Implement `on_error = "ask" | "rollback"`.
   - Keep localized retry/error behavior for STT, planner, capture, trace verification, and normal command failures, but add a global runebook error policy.
   - `ask`: pause execution and request an operator decision through the HUD/CLI before proceeding.
   - `rollback`: run explicit rollback steps when present; otherwise stop with a clear unsupported rollback error.
   - Include ask/rollback error policy decisions in the runebook trace.

3. Implement workflow composition as runebook functionality.
   - Add `parallel`, `race`, `batch`, `foreach`, `run`, and `spawn_run`.
   - Do not treat existing `InputAction::Sequence` as a general workflow engine; it only batches input actions.

4. Implement attestation and enrollment runebook fields.
   - Support `[attest]`.
   - Support `do = "attest"`.
   - Support machine identity status.
   - Support enrollment.
   - Support signed local-machine proofs.
   - Support cloud trust metadata.

5. Implement generic model steps in runebooks.
   - Support `do = "model"`.
   - Support `dispatch_model_action`.
   - Support model output spawning a child runebook.
   - Current source only has voice planner internals and `model eval`; expose the needed model surfaces through the runebook executor.

6. Expose voice/STT/planner turn steps through runebooks.
   - Existing text, WAV, and live-record turns in `frontends/cua-voice` should become programmable runebook steps.
   - Existing STT and planner config through voice CLI flags and env vars should be usable from `[stt.<name>]`, `[planner.<name>]`, and `[memory]`.
   - Add `do = "turn"`, STT-only steps, planner-only steps, and scheduled/programmatic model messages to the runebook executor.

7. Implement runebook trace verification and replay.
   - Existing trace start/inspect/verify/replay is action-trace oriented.
   - Add runebook-level traces, step-result traces, error-policy traces, verify-on-complete, replay, and shrinking.

8. Implement runebook conditions and timing.
   - Add `if`.
   - Add `if_present`.
   - Add result-based branching.
   - Add sleeps.
   - Add timers.
   - Add delayed messages.
   - Add first-class waits.

9. Extract a reusable `cua-client` SDK crate.
   - Keep `cua-core` as the protocol source of truth for request/response types.
   - Add remaining stable public API methods: `CuaClient::schemas()`, `CuaClient::acquire_owner(client_name, ttl_ms)`, `CuaClient::acquire_observer(client_name, ttl_ms)`, `CuaClient::visual_session(...)`, `CuaClient::pause()`, `CuaClient::resume()`, `CuaClient::kill_switch()`, `CuaClient::clipboard_read(...)`, and `CuaClient::clipboard_write(...)`.
   - Preserve idempotency support for input requests. Do not let SDK callers accidentally create untraceable duplicate actions.
   - Add typed error handling instead of returning generic `anyhow` strings for protocol errors.
   - Make long-lived CLI visual streaming use shared SDK session helpers instead of local socket glue.

10. Prefer Unix socket transport by default.
    - Keep HTTP as an optional operator/debug transport.
    - Use `session.acquire` for any SDK code path that mutates state.
    - Require an owner session for writes: input dispatch, frame input dispatch, profile create/activate, pause/resume/kill, and clipboard write.
    - Allow observer sessions for reads: status, manifest, schemas, screenshot, context, observe, events, and visual session.
    - Unix write paths already enforce session owner checks in `crates/cua-daemon/src/lib.rs`.
    - HTTP routes currently appear bearer-token-only and do not have the same owner-session write gate.
    - SDK mutation should prefer Unix until HTTP has equivalent session semantics.

11. Add first-class machine attestation types to `cua-core`.
    - Add `AttestationChallengeRequest` with `schema_version`, `audience`, optional `profile`, and optional requested claims.
    - Add `AttestationChallenge` with `schema_version`, `challenge_id`, `nonce`, `audience`, `issued_wall_ms`, and `expires_wall_ms`.
    - Add `MachineIdentity` with `schema_version`, `machine_key_id`, `machine_public_key`, `machine_id_hash`, `created_wall_ms`, and `key_backend`.
    - Add `RuntimeIdentityClaims` with `schema_version`, `runtime_name`, `runtime_version`, `daemon_pid`, `profile`, `socket_path`, `http_addr`, `bundle_id`, `designated_requirement`, `code_signature_summary`, `binary_sha256`, `permissions`, `active_profile`, `safety_state`, and `session_id`.
    - Add `MachineAttestation` with `schema_version`, `challenge`, `identity`, `claims`, `signature_algorithm`, `signature`, and `signed_wall_ms`.
    - Add all attestation types to `schema_bundle()`.
    - Update `tests/fixtures/schema-bundle.json`.
    - Do not expose raw hardware serials or platform UUIDs. Use a tenant/project/audience-salted machine hash.

12. Implement local machine identity storage.
    - Create a machine identity concern under `~/.cua/identity/`.
    - Store non-secret metadata in `~/.cua/identity/machine.json`.
    - Store the private key in macOS Keychain when possible.
    - Prefer Secure Enclave-backed keys when practical.
    - If Secure Enclave is not available, use a Keychain-protected signing key.
    - Record only public key and key metadata in files.
    - Add key rotation support: current key, previous keys, creation time, and revocation marker.
    - Add `cua identity status --json`.
    - Add `cua identity rotate --json`.
    - Add host-backed tests that prove the key persists across daemon restarts.
    - Use suggested config paths: `~/.cua/identity/machine.json`, `~/.cua/identity/keys/current.json`, and `~/.cua/identity/keys/previous/<key_id>.json`.

13. Add attestation protocol methods to the daemon.
    - Add HTTP endpoints: `POST /attestation/challenge`, `POST /attestation/sign`, and `GET /attestation/identity`.
    - Add Unix socket RPC methods: `attestation.challenge`, `attestation.sign`, and `attestation.identity`.
    - Add CLI commands: `cua attestation challenge --audience <audience> --json`, `cua attestation sign --audience <audience> --nonce <nonce> --json`, `cua attestation identity --json`, and `cua attestation verify <attestation.json> --json`.
    - Include nonce, audience, issued/expires timestamps, profile, session id when present, daemon pid, runtime version, socket path, HTTP addr, active profile/capabilities, safety state, permission report, code signing identity where available, binary hash where available, machine public key id, and salted machine id hash in the attestation document.
    - Refuse expired challenges.
    - Refuse audience mismatch.
    - Refuse unsigned or unknown machine keys.
    - Include freshness checks.
    - Keep challenge nonces single-use where practical.
    - Do not treat attestation as authorization by itself. It proves machine/runtime identity; profile/session policy still controls action authority.

14. Use existing package proof as attestation evidence.
    - Reuse the facts already proven by `scripts/host-package-proof.sh`: app path, bundle id, executable, code signature, designated requirement, daemon binary signature, voice binary signature, ctx binary signature, and usage descriptions.
    - Move the reusable proof logic into Rust or a small shared script that the daemon can call safely.
    - Add a `CodeIdentityClaims` type to `cua-core`.
    - Include package/code identity claims in attestation when running from the packaged app.
    - Include a lower-confidence development identity when running from `target/debug`.
    - Add a clear `runtime_integrity_level` field with `packaged_signed`, `development_signed`, `development_unsigned`, `unknown`.
    - Make verification policies able to require `packaged_signed` for cloud enrollment.

15. Add cloud enrollment and verification flow.
    - Add `cua enroll --audience quilt-cloud --json`.
    - Enrollment should generate or load machine identity, request a cloud challenge, sign the challenge locally, submit public key and attestation, receive an enrollment id, and store enrollment metadata under `~/.cua/cloud/`.
    - Store cloud enrollment data at `~/.cua/cloud/enrollments/<audience>.json`.
    - Support multiple audiences/tenants without overwriting identity.
    - Add revocation handling.
    - Add `cua enroll status --json`.
    - Add SDK method `client.attest({ audience, nonce })`.
    - Add Quilt-side verifier design: verify nonce, verify signature, verify public key enrollment, verify code identity policy, verify profile/capability policy, and verify timestamp freshness.

16. Define the public TypeScript SDK.
    - Decide the package name: `@quilt/cua`, `@cua/sdk`, or `cua-sdk`.
    - Connect to local Unix socket on macOS where supported.
    - Fall back to loopback HTTP where Unix socket is not practical.
    - Generate types from `/schemas` or checked-in schema bundle.
    - Support visual streaming.
    - Expose model-facing watch sessions that can be opened, closed, or bounded by duration.
    - Feed sampled visual frames into the planner loop without blocking low-latency action dispatch.
    - Keep the Unix visual session as the hot transport path and add explicit cancellation/backpressure.
    - Verify final actions against fresh stream/screenshot evidence before completion.
    - Support owner/observer session semantics.
    - Support attestation.
    - Support frame-relative input dispatch.
    - Support event long-polling/streaming.
    - Support the target API shape:

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

17. Define the public Python SDK.
    - Use the same transport and session semantics as TypeScript.
    - Include examples for local agent scripts and notebook use.

18. Normalize all config under `~/.cua/{concern}/**/*`.
    - Keep all durable cua runtime/config state under `~/.cua` by concern.
    - Do not assemble production paths ad hoc in each crate.
    - Use target layout:

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

    - Use profile-local environment loading only.
    - Add migration from old paths to new paths: `~/.cua/.env` -> `~/.cua/config/env`; `~/.cua/profiles/<profile>/chat.db`, `~/.cua/profiles/<profile>/ctx`, `~/.cua/profiles/<profile>/http.token`, and `~/.cua/profiles/<profile>/daemon.sock` stay valid.
    - Make scripts use `CUA_HOME` and concern-specific artifact paths.
    - Add first-class agent-authored scratchpad docs.
    - Support ephemeral scratchpads for short-lived run/session reasoning.
    - Support durable scratchpads for longer-standing project/profile notes.
    - Keep the scratchpad primitive intentionally lightweight for now: files the agent can write, read, and reference.
    - Update the agent system prompt/runtime instructions so the agent appends useful discoveries, decisions, environment facts, and project context that may help future work.
    - Keep memory appends selective and work-relevant so scratchpads stay useful instead of becoming noisy transcripts.
    - Append and retrieve scratchpad memory in parallel with other runtime work whenever possible, so memory hygiene does not add avoidable turn latency.
    - Make blocking memory reads explicit only when the next action truly depends on that memory.
    - Keep scratchpads profile-scoped by default, with path shapes `~/.cua/profiles/<profile>/scratchpads/ephemeral/` and `~/.cua/profiles/<profile>/scratchpads/durable/`.

19. Fix current config/path gaps.
    - In `crates/cua-cli/src/main.rs`, decide whether current directory `.env` should remain development-only.
    - In `crates/cua-platform-macos/src/lib.rs`, keep external `aegis_binary()` discovery for `~/.local/bin/aegis`, Homebrew, and `/usr/local/bin`, but make any cua-owned tool resolve under `~/.cua/bin/` or packaged sibling first.
    - In `crates/cua-platform-macos/src/lib.rs`, update `ctx_binary()` so production prefers packaged sibling or `~/.cua/bin/ctx` instead of falling back to `vendor/ctx/ctx`.
    - In `scripts/package-macos-app.sh`, allow build output to remain repo-local under `artifacts/cua/macos`, but ensure installed/runtime config does not depend on repo paths.
    - In `scripts/package-macos-app.sh`, consider copying bundled tools into `~/.cua/bin/` or documenting packaged sibling resolution.
    - In `scripts/release.sh`, keep `$HOME/Applications` as an install target if desired, but ensure runtime logs/proofs generated by the app use `~/.cua/artifacts/...`.
    - In `scripts/release.sh`, repo-local release artifacts under `artifacts/cua/release/<run_id>` may remain.
    - In host proof scripts, keep repo-local outputs under `artifacts/cua/...` for development tests, but add `CUA_HOME` support and/or mirror final proof artifacts under `~/.cua/artifacts/proofs/...` for installed runtime flows.
    - Update `docs/http-api.md` and `cua.md` to document the canonical `~/.cua/{concern}/**/*` layout.

20. Harden profile/session semantics for SDK users.
    - Make owner-session write enforcement consistent across transports.
    - Add HTTP session id support for write routes, or mark HTTP writes as development/operator-only.
    - Add SDK defaults: read-only observer by default and explicit `acquireOwner()` before mutations.
    - Add owner-session heartbeat/lease renewal.
    - Add lease expiry tests.
    - Add explicit refusal evidence for writes attempted without owner lease.
    - Add docs explaining profile policy vs bearer token vs owner session: bearer token authenticates local profile access, owner session authorizes mutation, profile policy controls capabilities, and attestation proves runtime/machine identity.

21. Make all documented functionality scriptable through the SDK.
    - Support status, manifest, schemas, and metrics.
    - Support screenshot and window capture.
    - Support context snapshot.
    - Support desktop, display, cursor, and window observation.
    - Support event snapshot, after, and wait.
    - Support visual session streaming.
    - Support accessibility permission request.
    - Support session acquire/cancel/status.
    - Support UI step, reply, mode, and island.
    - Support profile create/activate/status.
    - Support pause/resume/kill switch.
    - Support input dispatch for mouse move, mouse click, mouse drag, key press, key type, key paste, sequence, open app, shell exec, Aegis, and ctx.
    - Support frame-relative input dispatch.
    - Support clipboard read/write through explicit clipboard endpoints.
    - Support model eval only as an optional advanced surface.
    - Support trace verify/replay helpers.
    - Support attestation identity/challenge/sign/verify.

22. Add SDK and architecture docs.
    - Add `docs/sdk.md`.
    - Add `docs/attestation.md`.
    - Add `docs/config-home.md`.
    - Update `docs/http-api.md`.
    - Update `cua.md`.
    - Update `README.md`.

23. Add SDK examples.
    - Add a minimal local status check example.
    - Add an observe screenshot/context example.
    - Add a click using frame-relative coordinates example.
    - Add an acquire owner, run a sequence, release owner example.
    - Add a visual session and process frames example.
    - Add a show UI progress in the HUD example.
    - Add a read/write clipboard with explicit capability profile example.
    - Add a request attestation and verify it locally example.
    - Add an enroll with Quilt/cloud example.
    - Add a use Aegis through cua example.
    - Add a use ctx through cua example.

24. Add validation and release tests.
    - Add unit tests for shared path helpers with `CUA_HOME`.
    - Add unit tests for env loading precedence.
    - Add unit tests for attestation schema serialization.
    - Add unit tests for challenge expiry and audience mismatch.
    - Add unit tests for machine id hashing without raw hardware ids.
    - Add host proof for machine key persistence.
    - Add host proof for signed package attestation claims.
    - Add host proof for SDK owner-session mutation.
    - Add host proof for SDK observer read-only behavior.
    - Add host proof for config migration.
    - Add Fozzy deterministic scenario covering SDK status/context/dispatch.
    - Add Fozzy trace recording and replay for SDK action path.

25. Run required shipping commands before release.

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

26. Resolve SDK package naming.
    - Choose between `@quilt/cua`, `@cua/sdk`, and `cua-sdk`.

27. Resolve attestation backend policy.
    - Choose Secure Enclave first, Keychain first, or file key only for tests.

28. Resolve HTTP write-route policy.
    - Decide whether HTTP write routes should remain supported after SDK launch.

29. Resolve token override policy.
    - Decide whether `CUA_HTTP_TOKEN` should remain a production override or become dev/test only.

30. Resolve current-directory environment loading policy.
    - Decide whether current-directory `.env` loading should be removed entirely or kept as dev-only behavior.

31. Resolve ctx installation policy.
    - Decide whether `ctx` should be installed into `~/.cua/bin/ctx` or only resolved as a packaged sibling.

32. Resolve cloud enrollment ownership.
    - Decide whether cloud enrollment belongs in this repo or in Quilt cloud with a local cua client.
