Mission: finish cua for production macOS local use, with no false logic, no dead compatibility paths, and verified end-to-end behavior.

Shared coordination rules:

- Work only from current source, tests, docs, and live command output.
- Keep `cua-core` as the protocol source of truth.
- Prefer Unix socket hot paths for local runtime control.
- Keep CLI and HTTP/API exposure for core functionality without drifting from the canonical protocol.
- Mac only.
- Run focused tests plus relevant Fozzy or host proof for changed runtime behavior.
- When work is done and verified, delete it from this file with no replacement prose.

Agent B: Cloud Enrollment

Scope:
- Own Quilt/cloud enrollment metadata, verifier policy, enrollment CLI/HTTP/Unix methods, schemas, SDK helpers, and docs.

Work:
- Add cloud enrollment: `cua enroll --audience quilt-cloud --json`, `cua enroll status --json`, local storage at `~/.cua/cloud/enrollments/<audience>.json`, multiple audiences, and revocation handling.
- Add SDK helpers for enrollment status and enrollment-backed attestation.
- Add Quilt-side verifier design covering nonce, signature, public key enrollment, code identity policy, profile/capability policy, timestamp freshness, and revocation.

Done means:
- Enrollment works through CLI, Unix socket, HTTP, and SDK helpers.
- Verifier rejects unknown keys, revoked enrollments, bad audience, expired challenge, altered payload, and stale timestamp.
- Enrollment docs explain what is local runtime proof versus cloud authorization.
