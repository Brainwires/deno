// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// The confined side: corpus.ts runs this under a fixed, partial grant (see
// corpus.ts) and it drives every probe through ONE vector, printing one JSON
// line per probe. It never touches the OS itself beyond loading the probe;
// every attempt is native code (FFI, a Node-API addon, an addon inside an npm
// package, or an --allow-run child), which is what the kernel layer is for.
//
//   deno run <grant> host.ts <vector> [<label>]

import { createRequire } from "node:module";

const vector = Deno.args[0];
// the vector the rows and the probe's files are named for (a nested deno
// runs the FFI vector as "nested")
const label = Deno.args[1] ?? vector;
const work = new URL(".", import.meta.url).pathname;
const { cfg, probes } = JSON.parse(
  Deno.readTextFileSync(`${work}cfg.json`),
) as {
  cfg: string;
  probes: string[];
};
const conf = `${cfg};vec=${label}`;

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
    case "nested":
    case "nested-env":
    case "nested-closed": {
      // a deno this one starts with -A (the run grant names the runtime):
      // it inherits this run's grant and drives the FFI vector. nested-env
      // starts it through `env -i` (the variable cleared, the descriptor
      // open: found again); nested-closed through a shell that closes the
      // descriptor (grants unknown: it fails closed)
      const denoArgs = [
        "run",
        "--quiet",
        "--no-prompt",
        "-A",
        "--node-modules-dir=manual",
        `${work}host.ts`,
        "ffi",
        vector,
      ];
      const [program, args] = vector === "nested-env"
        ? ["/usr/bin/env", ["-i", Deno.execPath(), ...denoArgs]]
        : vector === "nested-closed"
        ? ["/bin/bash", [
          "-c",
          // the variable is pinned into every child's environment
          'eval "exec ${DENO_INTERNAL_KERNEL_POLICY:-999}<&-"; ' +
          'unset DENO_INTERNAL_KERNEL_POLICY; exec "$@"',
          "sh",
          Deno.execPath(),
          ...denoArgs,
        ]]
        : [Deno.execPath(), denoArgs];
      const o = new Deno.Command(program, {
        args,
        stdout: "piped",
        stderr: "piped",
      }).outputSync();
      const lines = new Map<string, string>();
      for (const line of new TextDecoder().decode(o.stdout).split("\n")) {
        if (!line.startsWith("{")) continue;
        const r = JSON.parse(line) as { id: string; out: string };
        lines.set(r.id, r.out);
      }
      // the line that says why it stopped (a nested deno with grants
      // unknown can't even read its configuration: NotCapable, saying so)
      const errLines = new TextDecoder().decode(o.stderr).trim().split("\n");
      const err = errLines.find((l) => /NotCapable.*grants unknown/.test(l)) ??
        errLines.slice(-1)[0];
      run = (id) => lines.get(id) ?? `n/a nested deno exited ${o.code}: ${err}`;
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
  console.log(JSON.stringify({ vector: label, id, out }));
}
