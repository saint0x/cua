# Attestation Policy

cua attestation is a local runtime statement signed by the machine identity. It proves that the daemon which produced the statement had access to the enrolled local machine key and reported its current runtime facts. It does not authorize input, clipboard, profile mutation, or cloud access by itself; those remain controlled by bearer auth, owner sessions, profile capabilities, and the remote verifier policy.

Production machine keys use the macOS Keychain backend. Secure Enclave remains a schema value for a future backend, but it is not the production policy until the daemon implements and proves that backend. File keys are allowed only for explicit tests and development fixtures.

Normal files under `~/.cua/identity/` may contain public key material, key ids, creation times, revocation markers, and other non-secret metadata. Private key material must not be written there by production code.

Cloud enrollment is owned by the local cua client. The client enrolls a public machine key for each audience and stores local enrollment metadata under `~/.cua/cloud/enrollments/<audience>.json`; Quilt/cloud verifies nonce freshness, signature validity, enrolled public key, runtime code identity, profile/capability policy, and timestamp freshness.
