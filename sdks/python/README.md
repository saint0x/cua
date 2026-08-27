# cua-sdk

Thin Python wrapper over the local `cua` binary, runebooks, and daemon protocol.

```python
from cua_sdk import Cua

cua = Cua.connect(profile="default")
print(cua.status())
print(cua.run("tests/fixtures/runebook-smoke.cua.toml"))
```

The SDK intentionally shells to `cua run` and protocol commands instead of owning a second runtime.
