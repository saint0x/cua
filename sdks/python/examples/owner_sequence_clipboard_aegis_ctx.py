import os

from cua_sdk import Cua


cua = Cua.connect(profile=os.environ.get("CUA_PROFILE", "default"))
owner = cua.acquire_owner("python owner sequence", ttl_ms=30000)

try:
    cua.create_profile(
        "python-sdk-example",
        mode="supervised",
        duration_ms=60000,
        capabilities={"clipboard": True},
        session=owner,
    )
    cua.activate_profile(session=owner)
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
    cua.clipboard_write("hello from cua", session=owner)
    print(cua.clipboard_read(True))
    print(cua.aegis(["--help"], session=owner))
    print(cua.ctx(["--help"], session=owner))
finally:
    cua.cancel_session(owner)
