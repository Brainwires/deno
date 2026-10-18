import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { UDP } = require("internal/test/binding").internalBinding("udp_wrap");

globalThis.onmessage = (event) => {
  const result = new UDP().open(event.data);
  globalThis.postMessage(result);
  globalThis.close();
};
