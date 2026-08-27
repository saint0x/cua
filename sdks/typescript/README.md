# @cua/sdk

Thin TypeScript wrapper over the local `cua` binary, runebooks, and daemon protocol.

```ts
import { Cua } from "@cua/sdk";

const cua = await Cua.connect({ profile: "default" });
console.log(await cua.status());
console.log(await cua.run("tests/fixtures/runebook-smoke.cua.toml"));
```

The SDK intentionally shells to `cua run` and protocol commands instead of owning a second runtime.
