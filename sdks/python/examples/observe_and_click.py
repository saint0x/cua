import os

from cua_sdk import Cua


cua = Cua.connect(profile=os.environ.get("CUA_PROFILE", "default"))
owner = cua.acquire_owner("python observe-and-click", ttl_ms=30000)

try:
    desktop = cua.observe()
    context = cua.context(max_width=1280, include_bytes=False)
    cua.ui_step(
        "Dispatching frame-relative click",
        task="SDK example",
        tool="cua",
        step_index=1,
        step_total=1,
    )
    cua.dispatch_frame(
        context["frame"]["envelope"],
        {"schema_version": "cua.v1", "kind": "mouse_click", "x": 420, "y": 240, "button": "left", "count": 1},
        session=owner,
    )
    cua.ui_reply(f"Observed desktop with {len(desktop.get('windows', []))} windows.")
finally:
    cua.cancel_session(owner)
