# Attestation

cua ships local machine identity and runtime attestation for the resident macOS daemon.

The identity key is profile-scoped under `~/.cua/identity` and is reused across restarts. The daemon exposes challenge/sign/identity over local HTTP and the profile Unix socket; the CLI exposes matching `cua attestation ...` commands plus local verification.

## Surfaces

- `GET /attestation/identity`
- `POST /attestation/challenge`
- `POST /attestation/sign`
- `UNIX attestation.identity`
- `UNIX attestation.challenge`
- `UNIX attestation.sign`
- `cua attestation identity --json`
- `cua attestation challenge --audience <audience> --json`
- `cua attestation sign --audience <audience> --nonce <nonce> --json`
- `cua attestation verify <attestation.json> --audience <audience> --json`
- `cua identity status --json`
- `cua identity rotate --json`

## Boundary

Attestation proves that a local cua runtime signed fresh runtime claims with its profile machine key. Claims include profile, daemon pid, socket path, HTTP address, active profile policy, safety state, permissions, and package/code identity facts when available.

Attestation is not authorization by itself. Authorization remains a local runtime decision based on bearer-token access, profile policy, and owner-session leases for writes.

Cloud enrollment is not shipped yet. The current release can sign and verify local runtime claims, but it does not persist Quilt/cloud enrollment records or implement revocation.
