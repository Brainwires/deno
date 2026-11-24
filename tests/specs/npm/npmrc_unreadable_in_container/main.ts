// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// Gives this process, and so the `deno install` it starts, no-new-privileges
// and a seccomp filter that allows every system call, then runs the install
// in a project whose `.npmrc` can't be read.
const libc = Deno.dlopen(
  "libc.so.6",
  {
    prctl: {
      parameters: ["i32", "u64", "u64", "u64", "u64"],
      result: "i32",
    },
  } as const,
);

Deno.mkdirSync("proj");
Deno.writeTextFileSync("proj/deno.json", "{}\n");
Deno.writeTextFileSync(
  "proj/.npmrc",
  "registry=https://registry.example.invalid/\n",
);
Deno.chmodSync("proj/.npmrc", 0o000);
let readable = true;
try {
  Deno.readTextFileSync("proj/.npmrc");
} catch {
  readable = false;
}
if (readable) {
  console.log("the .npmrc stayed readable: run this as a user that isn't root");
  Deno.exit(0);
}

const PR_SET_SECCOMP = 22;
const PR_SET_NO_NEW_PRIVS = 38;
const SECCOMP_MODE_FILTER = 2;
// one instruction: BPF_RET | BPF_K with SECCOMP_RET_ALLOW
const filter = new Uint8Array(8);
const instruction = new DataView(filter.buffer);
instruction.setUint16(0, 0x06, true);
instruction.setUint32(4, 0x7fff0000, true);
// struct sock_fprog { unsigned short len; struct sock_filter *filter; }
const prog = new Uint8Array(16);
const fprog = new DataView(prog.buffer);
fprog.setUint16(0, 1, true);
const address = (bytes: Uint8Array<ArrayBuffer>) =>
  BigInt(Deno.UnsafePointer.value(Deno.UnsafePointer.of(bytes)));
fprog.setBigUint64(8, address(filter), true);
const noNewPrivs = libc.symbols.prctl(PR_SET_NO_NEW_PRIVS, 1n, 0n, 0n, 0n);
const seccomp = libc.symbols.prctl(
  PR_SET_SECCOMP,
  BigInt(SECCOMP_MODE_FILTER),
  address(prog),
  0n,
  0n,
);
console.log(
  `no-new-privileges and a seccomp filter: ${
    noNewPrivs === 0 && seccomp === 0
  }`,
);

const output = new Deno.Command(Deno.execPath(), {
  args: ["install"],
  cwd: "proj",
  env: { NO_COLOR: "1" },
  stdout: "piped",
  stderr: "piped",
}).outputSync();
const stderr = new TextDecoder().decode(output.stderr);
console.log(
  `.npmrc treated as absent: ${stderr.includes("treated as absent")}`,
);
console.log(
  `.npmrc reported unreadable: ${
    !output.success && stderr.includes("Error loading .npmrc")
  }`,
);
// (the temp folder is removed by the harness: the file must be removable)
Deno.chmodSync("proj/.npmrc", 0o600);
