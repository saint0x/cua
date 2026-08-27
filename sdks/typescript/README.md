# @cua/sdk

Thin TypeScript wrapper over the local `cua` binary, runebooks, and daemon protocol.

```ts
import { Cua } from "@cua/sdk";

const cua = await Cua.connect({ profile: "default" });
console.log(await cua.status());
console.log(await cua.run("tests/fixtures/runebook-smoke.cua.toml"));
```

The SDK intentionally shells to `cua run` and protocol commands instead of owning a second runtime.

## Thin Runebook Helpers

Every helper either runs a compact Runebook step or sends a Runebook `rpc` step with an optional owner session id.

```ts
const cua = await Cua.connect({ profile: "default" });
const owner = await cua.acquireOwner("agent");
const context = await cua.context({ maxWidth: 1280, includeBytes: true });

await cua.uiStep({
  label: "Opening Notes",
  task: "Write note",
  tool: "Notes",
  stepIndex: 1,
  stepTotal: 2,
});

await cua.openApp("Notes", { session: owner });
await cua.dispatchFrame({
  session: owner,
  sourceFrame: context.frame.envelope,
  action: { schema_version: "cua.v1", action: "mouse_click", x: 420, y: 240, button: "left", count: 1 },
});
```

Available helpers include `manifest`, `schemas`, `metrics`, `status`, `configStatus`, `sessionStatus`, `acquireOwner`, `cancelSession`, `profileStatus`, `createProfile`, `activateProfile`, `requestAccessibility`, `observe`, `screenshot`, `windowCapture`, `context`, `events`, `uiStep`, `uiIsland`, `uiReply`, `uiMode`, `clipboardRead`, `clipboardWrite`, `pause`, `resume`, `killSwitch`, `dispatch`, `dispatchFrame`, `openApp`, `shell`, `aegis`, and `ctx`.
