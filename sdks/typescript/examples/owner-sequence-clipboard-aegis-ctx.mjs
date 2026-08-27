import { Cua } from "../dist/index.js";

const cua = await Cua.connect({ profile: process.env.CUA_PROFILE ?? "default" });
const owner = await cua.acquireOwner("typescript owner sequence", 30000);

try {
  await cua.createProfile(
    {
      name: "typescript-sdk-example",
      mode: "supervised",
      durationMs: 60000,
      capabilities: { clipboard: true },
    },
    owner,
  );
  await cua.activateProfile(owner);
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
  await cua.clipboardWrite("hello from cua", owner);
  console.log(await cua.clipboardRead(true));
  console.log(await cua.aegis(["--help"], { session: owner }));
  console.log(await cua.ctx(["--help"], { session: owner }));
} finally {
  await cua.cancelSession(owner);
}
