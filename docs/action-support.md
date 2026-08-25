# CUA Action Support Ledger

This ledger records proven behavior. Unknown cells must stay unknown until a host-backed oracle proves support.

| Platform | Capture | Input | Status | Evidence |
| --- | --- | --- | --- | --- |
| synthetic | PNG/JPEG frame generation | Refusal-only real input; daemon-owned clipboard behind profile grant | Proven for protocol smoke tests | Unit tests and CLI smoke tests |
| macOS | CoreGraphics display capture | CGEvent planned | Native still-frame capture and permission probes wired; input unproven | Local daemon smoke and unit tests |
