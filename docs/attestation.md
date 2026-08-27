# Attestation

Attestation is schema-defined in `cua-core`, but it is not a shipped runtime surface yet.

Current source exports JSON schemas for:

- `AttestationChallengeRequest`
- `AttestationChallenge`
- `MachineIdentity`
- `RuntimeIdentityClaims`
- `MachineAttestation`

The CLI does not currently expose `cua attestation ...`, `cua identity ...`, or `cua enroll ...` commands. The daemon does not currently expose attestation HTTP routes or Unix RPC methods. The TypeScript and Python SDKs do not currently expose attestation or enrollment helpers.

## Schema Boundary

The schema models a local cua runtime signing a fresh challenge with a machine key and attaching runtime facts such as profile, daemon pid, socket path, HTTP address, active profile policy, safety state, permissions, and code identity facts when available.

Attestation must not be treated as authorization by itself. Authorization remains a separate local runtime decision based on profile policy, bearer-token access to the local daemon, and owner-session leases for writes.

## Current Threat Model

The checked-in schema shape is suitable for verifier design and schema compatibility tests only. It does not yet prove:

- persistent machine key storage
- challenge freshness enforcement
- challenge replay prevention
- signed runtime claims
- enrollment with Quilt/cloud
- local verification of an attestation file

Do not build production authorization or cloud enrollment flows on this surface in the current release. The required CLI, daemon, Unix RPC, SDK helpers, and verifier rejection tests do not exist.
