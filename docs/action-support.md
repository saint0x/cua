# CUA Action Support Ledger

This ledger records proven behavior. Unknown cells must stay unknown until a host-backed oracle proves support.

| Platform | Capture | Input | Status | Evidence |
| --- | --- | --- | --- | --- |
| synthetic | PNG/JPEG frame generation | Refusal-only real input; daemon-owned clipboard behind profile grant | Proven for protocol smoke tests | Unit tests and CLI smoke tests |
| macOS | CoreGraphics display, cursor, window, and still-frame capture | CGEvent mouse and keyboard | Native observation, still-frame capture, input dispatch, and permission probes wired | Local daemon observe smoke and unit tests |
