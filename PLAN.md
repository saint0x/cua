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
