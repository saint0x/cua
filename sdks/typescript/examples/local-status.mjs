import { Cua } from "../dist/index.js";

const cua = await Cua.connect({ profile: process.env.CUA_PROFILE ?? "default" });

console.log(await cua.status());
