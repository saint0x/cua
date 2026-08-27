# cua-sdk

Thin Python wrapper over the local `cua` binary, runebooks, and daemon protocol.

```python
from cua_sdk import Cua

cua = Cua.connect(profile="default")
print(cua.status())
print(cua.run("tests/fixtures/runebook-smoke.cua.toml"))
```

The SDK intentionally shells to `cua run` and protocol commands instead of owning a second runtime.

## Thin Runebook Helpers

Every helper either runs a compact Runebook step or sends a Runebook `rpc` step with an optional owner session id.

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

Available helpers include `manifest`, `schemas`, `metrics`, `status`, `config_status`, `session_status`, `acquire_owner`, `cancel_session`, `profile_status`, `create_profile`, `activate_profile`, `request_accessibility`, `observe`, `screenshot`, `window_capture`, `context`, `events`, `ui_step`, `ui_island`, `ui_reply`, `ui_mode`, `clipboard_read`, `clipboard_write`, `pause`, `resume`, `kill_switch`, `dispatch`, `dispatch_frame`, `open_app`, `shell`, `aegis`, and `ctx`.
