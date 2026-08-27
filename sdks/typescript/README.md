# @cua/sdk

Thin TypeScript wrapper over the local `cua` binary, runebooks, and daemon protocol.

```ts
import { Cua } from "@cua/sdk";

const cua = await Cua.connect({ profile: "default" });
console.log(await cua.status());
console.log(await cua.run("tests/fixtures/runebook-smoke.cua.toml"));
```

The SDK uses the local Unix socket for protocol RPC by default and falls back to `cua run` when the profile socket is unavailable.

## Thin Runebook Helpers

Every helper either runs a compact Runebook step or sends a Runebook `rpc` step. Read helpers work without ownership; mutation helpers require an explicit owner session so callers do not create unleased actions by accident.

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

Use `heartbeatOwner(owner, ttlMs)` to renew a lease while a long-running controller is active.

Available helpers include `manifest`, `schemas`, `metrics`, `status`, `configStatus`, `sessionStatus`, `acquireOwner`, `heartbeatOwner`, `cancelSession`, `profileStatus`, `createProfile`, `activateProfile`, `requestAccessibility`, `attest`, `observe`, `screenshot`, `windowCapture`, `context`, `events`, `visualFrames`, `uiStep`, `uiIsland`, `uiReply`, `uiMode`, `clipboardRead`, `clipboardWrite`, `pause`, `resume`, `killSwitch`, `dispatch`, `dispatchFrame`, `openApp`, `shell`, `aegis`, `ctx`, `traceVerify`, `traceReplay`, and `modelEval`.
