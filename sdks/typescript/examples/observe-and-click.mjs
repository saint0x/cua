import { Cua } from "../dist/index.js";

const cua = await Cua.connect({ profile: process.env.CUA_PROFILE ?? "default" });
const owner = await cua.acquireOwner("typescript observe-and-click", 30000);

try {
  const desktop = await cua.observe();
  const context = await cua.context({ maxWidth: 1280, includeBytes: false });
  await cua.uiStep({
    label: "Dispatching frame-relative click",
    task: "SDK example",
    tool: "cua",
    stepIndex: 1,
    stepTotal: 1,
  });
  await cua.dispatchFrame({
    session: owner,
    sourceFrame: context.frame.envelope,
    action: { schema_version: "cua.v1", kind: "mouse_click", x: 420, y: 240, button: "left", count: 1 },
  });
  await cua.uiReply({ text: `Observed desktop with ${desktop.windows?.length ?? 0} windows.` });
} finally {
  await cua.cancelSession(owner);
}
