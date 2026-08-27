# cua-sdk

Thin Python wrapper over the local `cua` binary, runebooks, and daemon protocol.

```python
from cua_sdk import Cua

cua = Cua.connect(profile="default")
print(cua.status())
print(cua.run("tests/fixtures/runebook-smoke.cua.toml"))
```

The SDK uses the local Unix socket for protocol RPC by default and falls back to `cua run` when the profile socket is unavailable.

Reads are available after `connect`. For protected writes, acquire an owner session and pass it to mutation helpers. Clipboard access also requires an active profile with clipboard capability.

Runnable examples live in `sdks/python/examples`.

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
    {"schema_version": "cua.v1", "kind": "mouse_click", "x": 420, "y": 240, "button": "left", "count": 1},
    session=owner,
)
```

Use `heartbeat_owner(owner, ttl_ms)` to renew a lease while a long-running controller is active.

Available helpers include `manifest`, `schemas`, `metrics`, `status`, `config_status`, `session_status`, `acquire_owner`, `heartbeat_owner`, `cancel_session`, `profile_status`, `create_profile`, `activate_profile`, `request_accessibility`, `attest`, `observe`, `screenshot`, `window_capture`, `context`, `events`, `visual_frames`, `ui_step`, `ui_island`, `ui_reply`, `ui_mode`, `clipboard_read`, `clipboard_write`, `pause`, `resume`, `kill_switch`, `dispatch`, `dispatch_frame`, `open_app`, `shell`, `aegis`, `ctx`, `trace_verify`, `trace_replay`, and `model_eval`.

## Examples

Minimal local status check:

```python
cua = Cua.connect(profile="default")
print(cua.status())
```

Observe screenshot/context:

```python
cua = Cua.connect()
desktop = cua.observe()
context = cua.context(max_width=1280, include_bytes=False)
print({"desktop": desktop, "frame": context})
```

Click using frame-relative coordinates:

```python
cua = Cua.connect()
owner = cua.acquire_owner("python example")
context = cua.context(max_width=1280, include_bytes=False)
cua.dispatch_frame(
    context["frame"]["envelope"],
    {"schema_version": "cua.v1", "kind": "mouse_click", "x": 420, "y": 240, "button": "left", "count": 1},
    session=owner,
)
cua.cancel_session(owner)
```

Acquire owner, run a sequence, release owner:

```python
cua = Cua.connect()
owner = cua.acquire_owner("python sequence")
cua.dispatch(
    {
        "schema_version": "cua.v1",
        "kind": "sequence",
        "actions": [
            {"kind": "open_app", "app_name": "Notes"},
            {"kind": "key_press", "combo": "cmd+n"},
        ],
        "inter_action_delay_ms": 120,
    },
    session=owner,
)
cua.cancel_session(owner)
```

Show UI progress in the HUD:

```python
cua.ui_step("Checking desktop", task="SDK example", tool="cua", step_index=1, step_total=1)
cua.ui_reply("Desktop check complete.")
```

Read/write clipboard with explicit capability profile:

```python
owner = cua.acquire_owner("python clipboard")
cua.create_profile("clipboard-example", mode="supervised", duration_ms=60000, capabilities={"clipboard": True}, session=owner)
cua.activate_profile(session=owner)
cua.clipboard_write("hello from cua", session=owner)
print(cua.clipboard_read(True))
cua.cancel_session(owner)
```

Use Aegis through cua:

```python
owner = cua.acquire_owner("python aegis")
cua.aegis(["--help"], session=owner)
cua.cancel_session(owner)
```

Use ctx through cua:

```python
owner = cua.acquire_owner("python ctx")
cua.ctx(["--help"], session=owner)
cua.cancel_session(owner)
```
