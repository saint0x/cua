# CUA Action Support Ledger

This ledger records proven behavior. Unknown cells must stay unknown until a host-backed oracle proves support.

| Platform | Capture | Input | Status | Evidence |
| --- | --- | --- | --- | --- |
| synthetic | PNG/JPEG frame generation | Refusal-only real input; daemon-owned clipboard behind profile grant | Proven for protocol smoke tests | Unit tests and CLI smoke tests |
| macOS | ScreenCaptureKit-backed display capture with CoreGraphics fallback; native cursor/window observation | CGEvent mouse and keyboard | Native capture, observation, input dispatch, and permission probes wired | Local daemon observe/capture smoke and unit tests |
