// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// The confined side: corpus.ts runs this under a fixed, partial grant (see
// corpus.ts) and it drives every probe through ONE vector, printing one JSON
// line per probe. It never touches the OS itself beyond loading the probe;
// every attempt is native code (FFI, a Node-API addon, an addon inside an npm
// package, or an --allow-run child), which is what the kernel layer is for.
//
//   deno run <grant> host.ts <vector>

import { createRequire } from "node:module";

const vector = Deno.args[0];
const work = new URL(".", import.meta.url).pathname;
const { cfg, probes } = JSON.parse(
  Deno.readTextFileSync(`${work}cfg.json`),
) as {
  cfg: string;
  probes: string[];
};
const conf = `${cfg};vec=${vector}`;

let run: (id: string) => string;
try {
  switch (vector) {
    case "ffi": {
      const ext = Deno.build.os === "darwin" ? "dylib" : "so";
      const lib = Deno.dlopen(`${work}libprobe.${ext}`, {
        corpus_probe: {
          parameters: ["buffer", "buffer", "buffer", "i32"],
          result: "i32",
        },
      });
      const enc = new TextEncoder();
      run = (id) => {
        const out = new Uint8Array(1024);
        lib.symbols.corpus_probe(
          enc.encode(id + "\0"),
          enc.encode(conf + "\0"),
          out,
          out.length,
        );
        return new TextDecoder().decode(out.subarray(0, out.indexOf(0)));
      };
      break;
    }
    case "napi": {
      const addon = createRequire(import.meta.url)("./probe.node");
      run = (id) => addon.probe(id, conf);
      break;
    }
    case "npm": {
      const addon = createRequire(import.meta.url)("corpus-native");
      run = (id) => addon.probe(id, conf);
      break;
    }
    case "child": {
      run = (id) => {
        const o = new Deno.Command(`${work}probe`, {
          args: [id, conf],
          stdout: "piped",
          stderr: "piped",
        })
          .outputSync();
        const text = new TextDecoder().decode(o.stdout).trim();
        return text ||
          `n/a child exited ${o.code}: ${
            new TextDecoder().decode(o.stderr).trim()
          }`;
      };
      break;
    }
    default:
      throw new Error(`unknown vector ${vector}`);
  }
} catch (e) {
  // The vector itself could not load (a refusal is a result too).
  const why = (e as Error).message.split("\n")[0];
  run = () => `n/a vector could not load: ${why}`;
}

for (const id of probes) {
  let out: string;
  try {
    out = run(id);
  } catch (e) {
    out = `n/a vector failed: ${(e as Error).message}`;
  }
  console.log(JSON.stringify({ vector, id, out }));
}
