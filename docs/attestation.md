# Attestation

cua ships machine identity and runtime attestation for the selected computer backend. The default installed backend is the resident local macOS computer.

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

Attestation proves that a cua runtime signed fresh runtime claims with its profile machine key. Claims include profile, daemon pid, socket path, HTTP address, selected `computer_backend`, active profile policy, safety state, permissions, and package/code identity facts when available.

Attestation is not authorization by itself. Authorization remains a local runtime decision based on bearer-token access, profile policy, and owner-session leases for writes.

The current release signs and verifies runtime claims for the selected backend, including `remote_cua`, `oracle-vm`, and the qgui-backed Oracle VM node backend when selected through environment configuration. Oracle Cloud Infrastructure lifecycle operations are CLI-backed; a launched VM instance becomes controllable after it exposes a CUA HTTP endpoint and token. Durable Quilt/cloud fleet enrollment persistence and revocation are still outside the shipped attestation surface.
