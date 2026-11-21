// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// The confined parent (a worker under the supervising broker): starts
// `deno` children with -A, which the inherited grant narrows to its own.
const [port, otherPort, mode] = Deno.args;
const deno = Deno.execPath();

async function child(label: string, command: string, args: string[]) {
  const { stdout, stderr } = await new Deno.Command(command, { args })
    .output();
  const text = new TextDecoder().decode(stdout) +
    new TextDecoder().decode(stderr);
  for (const line of text.trim().split("\n")) {
    console.log(`${label}: ${line}`);
  }
}

if (mode === "narrower") {
  // a child with a kernel sandbox of its own, narrower than this one (no
  // write, no network); it is told which descriptors the dial is here
  await child("narrower child", deno, [
    "run",
    "--quiet",
    "--no-prompt",
    "--kernel-sandbox=enforce",
    "--allow-ffi",
    "--allow-env=DENO_INTERNAL_KERNEL_BROKER_DIAL",
    "--allow-read=.",
    "brokered_narrow.ts",
    Deno.env.get("DENO_INTERNAL_KERNEL_BROKER_DIAL") ?? "",
  ]);
} else {
  await child("child", deno, [
    "run",
    "--quiet",
    "-A",
    "brokered_child.ts",
    port,
    otherPort,
    "bout/child.txt",
  ]);
  // through a shell: the way to the broker is inherited, not passed by
  // this process to a `deno` it recognizes
  await child("grandchild through sh", "sh", [
    "-c",
    `"$0" run --quiet -A brokered_child.ts ${port} ${otherPort} bout/grandchild.txt`,
    deno,
  ]);
  // the parent's own operations are untouched by its children's
  const conn = await Deno.connect({ hostname: "localhost", port: Number(port) });
  await conn.write(new TextEncoder().encode("self"));
  const buf = new Uint8Array(4);
  await conn.read(buf);
  conn.close();
  console.log(`parent: connect after its children: ${new TextDecoder().decode(buf)}`);
}
