# cua SDK

The canonical programmable surface is the cua runebook plus the local daemon protocol. SDKs are thin convenience layers over that surface.

## Rust

`crates/cua-client` is the shared Rust client for the profile-local Unix socket.

```rust
let client = cua_client::CuaClient::connect("default").await?;
let status: serde_json::Value = client.request("status", None).await?;
```

The voice app and CLI use this crate for normal one-shot Unix RPC calls.

## TypeScript

The local TypeScript package lives at `sdks/typescript`.

```ts
import { Cua } from "@cua/sdk";

const cua = await Cua.connect({ profile: "default" });
await cua.run("tests/fixtures/runebook-smoke.cua.toml");
console.log(await cua.configStatus());
```

## Python

The local Python package lives at `sdks/python`.

```python
from cua_sdk import Cua

cua = Cua.connect(profile="default")
print(cua.run("tests/fixtures/runebook-smoke.cua.toml"))
print(cua.config_status())
```

## Design Rule

Do not implement a second runtime in a language SDK. The SDK should call `cua run`, typed CLI commands, or the daemon protocol, and the runebook/protocol layer remains the source of truth.
