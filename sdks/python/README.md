# cua-sdk

Thin Python wrapper over the local `cua` binary, runebooks, and daemon protocol.

```python
from cua_sdk import Cua

cua = Cua.connect(profile="default")
print(cua.status())
print(cua.run("tests/fixtures/runebook-smoke.cua.toml"))
```

The SDK uses the local Unix socket for protocol RPC by default and falls back to `cua run` when the profile socket is unavailable.

## Thin Runebook Helpers

Every helper either runs a compact Runebook step or sends a Runebook `rpc` step. Read helpers work without ownership; mutation helpers require an explicit owner session so callers do not create unleased actions by accident.

```python
from cua_sdk import Cua

cua = Cua.connect(profile="default")
owner = cua.acquire_owner("agent")
context = cua.context(max_width=1280, include_bytes=True)

cua.ui_step(
    "Opening Notes",
    task="Write note",
    tool="Notes",
    step_index=1,
    step_total=2,
)

cua.open_app("Notes", session=owner)
cua.dispatch_frame(
    context["frame"]["envelope"],
    {"schema_version": "cua.v1", "action": "mouse_click", "x": 420, "y": 240, "button": "left", "count": 1},
    session=owner,
)
```

Use `heartbeat_owner(owner, ttl_ms)` to renew a lease while a long-running controller is active.

Available helpers include `manifest`, `schemas`, `metrics`, `status`, `config_status`, `session_status`, `acquire_owner`, `heartbeat_owner`, `cancel_session`, `profile_status`, `create_profile`, `activate_profile`, `request_accessibility`, `attest`, `observe`, `screenshot`, `window_capture`, `context`, `events`, `visual_frames`, `ui_step`, `ui_island`, `ui_reply`, `ui_mode`, `clipboard_read`, `clipboard_write`, `pause`, `resume`, `kill_switch`, `dispatch`, `dispatch_frame`, `open_app`, `shell`, `aegis`, `ctx`, `trace_verify`, `trace_replay`, and `model_eval`.
