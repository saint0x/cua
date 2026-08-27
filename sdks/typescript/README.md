# @cua/sdk

Thin TypeScript wrapper over the local `cua` binary, runebooks, and daemon protocol.

```ts
import { Cua } from "@cua/sdk";

const cua = await Cua.connect({ profile: "default" });
console.log(await cua.status());
console.log(await cua.run("tests/fixtures/runebook-smoke.cua.toml"));
```

The SDK uses the local Unix socket for protocol RPC by default and falls back to `cua run` when the profile socket is unavailable.

Reads are available after `connect`. For protected writes, acquire an owner session and pass it to mutation helpers. Clipboard access also requires an active profile with clipboard capability.

Runnable examples live in `sdks/typescript/examples` after `npm --prefix sdks/typescript run build`.

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
  action: { schema_version: "cua.v1", kind: "mouse_click", x: 420, y: 240, button: "left", count: 1 },
});
```

Use `heartbeatOwner(owner, ttlMs)` to renew a lease while a long-running controller is active.

Available helpers include `manifest`, `schemas`, `metrics`, `status`, `configStatus`, `sessionStatus`, `acquireOwner`, `heartbeatOwner`, `cancelSession`, `profileStatus`, `createProfile`, `activateProfile`, `requestAccessibility`, `attest`, `observe`, `screenshot`, `windowCapture`, `context`, `events`, `visualFrames`, `uiStep`, `uiIsland`, `uiReply`, `uiMode`, `clipboardRead`, `clipboardWrite`, `pause`, `resume`, `killSwitch`, `dispatch`, `dispatchFrame`, `openApp`, `shell`, `aegis`, `ctx`, `traceVerify`, `traceReplay`, and `modelEval`.

## Examples

Minimal local status check:

```ts
const cua = await Cua.connect({ profile: "default" });
console.log(await cua.status());
```

Observe screenshot/context:

```ts
const cua = await Cua.connect();
const desktop = await cua.observe();
const context = await cua.context({ maxWidth: 1280, includeBytes: false }) as any;
console.log({ desktop, frame: context });
```

Click using frame-relative coordinates:

```ts
const cua = await Cua.connect();
const owner = await cua.acquireOwner("typescript example");
const context = await cua.context({ maxWidth: 1280, includeBytes: false });
await cua.dispatchFrame({
  session: owner,
  sourceFrame: context.frame.envelope,
  action: { schema_version: "cua.v1", kind: "mouse_click", x: 420, y: 240, button: "left", count: 1 },
});
await cua.cancelSession(owner);
```

Acquire owner, run a sequence, release owner:

```ts
const cua = await Cua.connect();
const owner = await cua.acquireOwner("typescript sequence");
await cua.dispatch(
  {
    schema_version: "cua.v1",
    kind: "sequence",
    actions: [
      { kind: "open_app", app_name: "Notes" },
      { kind: "key_press", combo: "cmd+n" },
    ],
    inter_action_delay_ms: 120,
  },
  { session: owner },
);
await cua.cancelSession(owner);
```

Show UI progress in the HUD:

```ts
await cua.uiStep({ label: "Checking desktop", task: "SDK example", tool: "cua", stepIndex: 1, stepTotal: 1 });
await cua.uiReply({ text: "Desktop check complete." });
```

Read/write clipboard with explicit capability profile:

```ts
const owner = await cua.acquireOwner("typescript clipboard");
await cua.createProfile({ name: "clipboard-example", mode: "supervised", durationMs: 60000, capabilities: { clipboard: true } }, owner);
await cua.activateProfile(owner);
await cua.clipboardWrite("hello from cua", owner);
console.log(await cua.clipboardRead(true));
await cua.cancelSession(owner);
```

Use Aegis through cua:

```ts
const owner = await cua.acquireOwner("typescript aegis");
await cua.aegis(["--help"], { session: owner });
await cua.cancelSession(owner);
```

Use ctx through cua:

```ts
const owner = await cua.acquireOwner("typescript ctx");
await cua.ctx(["--help"], { session: owner });
await cua.cancelSession(owner);
```
