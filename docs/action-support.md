# cua Action Support Ledger

This ledger records proven behavior. Unknown cells must stay unknown until a host-backed oracle proves support.

| Platform | Capture | Input | Status | Evidence |
| --- | --- | --- | --- | --- |
| synthetic | PNG/JPEG frame generation | Refusal-only real input and clipboard | Proven for protocol smoke tests | Unit tests and CLI smoke tests |
| macOS | ScreenCaptureKit-backed display capture with CoreGraphics fallback; native cursor/window observation | CGEvent mouse and keyboard | Native capture, observation, input dispatch, and permission probes wired | Host GUI smoke, trace verify, and unit tests |
| remote CUA | Proxies `/capture/screenshot` and `/observe/desktop` from a remote CUA daemon | Proxies `/input/dispatch` with bearer auth and owner session lease | Real control adapter when `CUA_REMOTE_CUA_URL` and `CUA_REMOTE_CUA_TOKEN` are configured | Unit tests plus live two-daemon HTTP proxy smoke for observe and input dispatch |
| Oracle VM | qgui/KasmVNC/XFCE capture through bundled `cua-qgui-tool`; lifecycle provider still uses the Oracle Cloud Infrastructure CLI | qgui/XTEST mouse and keyboard plus persistent X11 clipboard through bundled `cua-qgui-tool` | Live Oracle VM node backend wired as `ComputerBackendKind::OracleVm` with `runtime=qgui+cua` | Live Oracle VM ARM build, qgui systemd service, daemon status, observe, screenshot, mouse readback, and clipboard write/readback |

All rows are exposed through the general computer backend boundary. Local macOS is the default installed backend. Quilt VM remains intentionally absent from this ledger until a real backend implementation and host-backed trace prove capture, observe, input, safety, and attestation behavior.
