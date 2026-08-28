# cua Action Support Ledger

This ledger records proven behavior. Unknown cells must stay unknown until a host-backed oracle proves support.

| Platform | Capture | Input | Status | Evidence |
| --- | --- | --- | --- | --- |
| synthetic | PNG/JPEG frame generation | Refusal-only real input; daemon-owned clipboard behind profile grant | Proven for protocol smoke tests | Unit tests and CLI smoke tests |
| macOS | ScreenCaptureKit-backed display capture with CoreGraphics fallback; native cursor/window observation | CGEvent mouse and keyboard | Native capture, observation, input dispatch, and permission probes wired | Host GUI smoke, trace verify, and unit tests |
| remote CUA | Proxies `/capture/screenshot` and `/observe/desktop` from a remote CUA daemon | Proxies `/input/dispatch` with bearer auth and owner session lease | Real control adapter when `CUA_REMOTE_CUA_URL` and `CUA_REMOTE_CUA_TOKEN` are configured | Unit tests plus live two-daemon HTTP proxy smoke for observe and input dispatch |
| Oracle OCI | CLI-backed authentication, availability-domain discovery, instance launch/status/terminate provider | No direct desktop input until the OCI VM exposes a CUA endpoint | Lifecycle provider wired; control is unavailable without remote CUA endpoint metadata | Live `cua cloud oci doctor`, availability-domain lookup, instance launch/status, SSH bootstrap, ARM Linux build/clippy, and live daemon unavailable-backend probe |

All rows are exposed through the general computer backend boundary. Local macOS is the default installed backend. Quilt VM remains intentionally absent from this ledger until a real backend implementation and host-backed trace prove capture, observe, input, safety, and attestation behavior.
