# CUA Action Support Ledger

This ledger records proven behavior. Unknown cells must stay unknown until a host-backed oracle proves support.

| Platform | Capture | Input | Status | Evidence |
| --- | --- | --- | --- | --- |
| synthetic | PNG/JPEG frame generation | Refusal-only real input; daemon-owned clipboard behind profile grant | Proven for protocol smoke tests | Unit tests and CLI smoke tests |
| macOS | ScreenCaptureKit planned | CGEvent planned | Unproven | Pending host-backed tests |
| Windows | Graphics Capture planned | SendInput planned | Unproven | Pending host-backed tests |
| Linux X11 | X11 capture planned | XTest/uinput planned | Unproven | Pending host-backed tests |
| Linux Wayland | PipeWire/XDG portal planned | Portal/libei/compositor-mediated planned | Unproven/refusal expected where denied | Pending host-backed tests |
