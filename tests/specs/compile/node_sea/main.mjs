import sea from "node:sea";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
console.log("same module:", require("node:sea").isSea === sea.isSea);
console.log("isSea:", sea.isSea());

function code(fn) {
  try {
    fn();
    return "no error";
  } catch (err) {
    return err.code;
  }
}

if (!sea.isSea()) {
  console.log("getAsset:", code(() => sea.getAsset("assets/greeting.txt")));
} else {
  const text = sea.getAsset("assets/greeting.txt", "utf8");
  console.log("text:", JSON.stringify(text));
  const raw = sea.getRawAsset("assets/greeting.txt");
  console.log("raw:", raw instanceof ArrayBuffer, raw.byteLength);
  const copy = sea.getAsset("assets/greeting.txt");
  console.log("copy:", copy instanceof ArrayBuffer, copy !== raw);
  const blob = sea.getAssetAsBlob("assets/greeting.txt", {
    type: "text/plain",
  });
  console.log("blob:", blob.type, JSON.stringify(await blob.text()));
  console.log("missing:", code(() => sea.getAsset("assets/missing.txt")));
  // Only the program's embedded files: never a file on disk.
  console.log("outside:", code(() => sea.getAsset("/etc/hosts")));
  console.log("escape:", code(() => sea.getAsset("../../../../etc/hosts")));
  console.log("bad key:", code(() => sea.getAsset(1)));
}
