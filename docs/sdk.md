# cua SDK

The canonical programmable surface is the cua runebook plus the local daemon protocol. SDKs are thin convenience layers over that surface.

Reads are available immediately after connecting. Mutations should acquire an owner session first and pass that session to write helpers. The bearer token authenticates access to the local daemon; it is not an owner lease. Profile policy still decides whether capabilities such as clipboard are granted.

Status and session inventory responses include `computer_backend`, which identifies the selected computer substrate. A default install reports the local macOS backend. Remote CUA and Oracle VM-backed computers preserve the same SDK-facing protocol instead of adding a parallel runtime.

## Rust

`crates/cua-client` is the shared Rust client for the profile-local Unix socket.

```rust
let client = cua_client::CuaClient::connect("default").await?;
let status: serde_json::Value = client.request("status", None).await?;
```

The voice app and CLI use this crate for normal one-shot Unix RPC calls.

Rust is currently the SDK surface with direct persistent visual-session support through `CuaClient::visual_session(...)`.

```rust
let client = cua_client::CuaClient::connect("default").await?;
let mut stream = client.visual_session(1280, 10, false, None).await?;
while let Some(frame) = stream.next_frame().await? {
    println!("frame {} {}x{}", frame.frame_id, frame.width, frame.height);
    break;
}
stream.close().await?;
```

Rust also exposes typed helpers for inbox/webhook and attestation over the profile Unix socket:

```rust
let status = client.inbox_after(0).await?;
```

## TypeScript

The local TypeScript package lives at `sdks/typescript`.

```ts
import { Cua } from "@cua/sdk";

const cua = await Cua.connect({ profile: "default" });
await cua.run("tests/fixtures/runebook-smoke.cua.toml");
console.log(await cua.configStatus());
await cua.inboxPublish("what do you see on my screen?");
```

## Python

The local Python package lives at `sdks/python`.

```python
from cua_sdk import Cua

cua = Cua.connect(profile="default")
print(cua.run("tests/fixtures/runebook-smoke.cua.toml"))
print(cua.config_status())
print(cua.inbox_publish("what do you see on my screen?"))
```

The TypeScript and Python packages shell to `cua run`, typed CLI commands, and Unix RPC. They expose one-shot context/screenshot helpers, frame-relative dispatch, owner sessions, local attestation signing, backend status discovery, and inbox/webhook helpers. Rust exposes persistent visual sessions directly. Oracle VM node control is available through the same daemon protocol once a launched instance exposes its CUA endpoint. Fleet enrollment helpers for durable pool membership, provider rotation, and revocation are intentionally outside the SDK surface for now; the CLI exposes `cua cloud oci doctor`, `cua cloud oci availability-domains`, `cua cloud oci launch`, `cua cloud oci status`, and `cua cloud oci terminate` for Oracle Cloud Infrastructure-backed fleet operations.

## Examples And Proofs

The SDK README files and `sdks/*/examples` include examples for shipped helpers. Those examples are backed by the runebook fixtures under `tests/fixtures/`, daemon unit tests for owner-session refusal, inbox/webhook behavior, and clipboard policy, plus host proofs such as `scripts/host-session-proof.sh` and `scripts/host-control-surface-proof.sh`.

## Design Rule

Do not implement a second runtime in a language SDK. The SDK should call `cua run`, typed CLI commands, or the daemon protocol, and the runebook/protocol layer remains the source of truth.
