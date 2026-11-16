// Copyright 2018-2026 the Deno authors. MIT license.
// What --expect=enforce allows per check, derived from probes.ts: a cell
// whose column says "kernel" must be held (an "open" there is an escape);
// "coarser" / "deno-only" / "by-design" cells may come back either way,
// as documented, except where an enforce configuration decides them:
//
// - files (no network): every network cell is held: no IP family, no Unix
//   socket, no netlink beyond what isn't granted (route included).
// - net-all: the IP cells are granted (open; held would be a wrong
//   refusal), Unix sockets, the session bus and non-route netlink stay held.
// - no --allow-run (files, net-all): native code can't start programs
//   (exec.run-list held for the in-process vectors; the child vector can't
//   load). With a run list: by design.
// - fs.meta under enforce: the path forms are refused (Linux seccomp,
//   macOS Seatbelt): held.
// - env: the re-executed image's environment is scrubbed to what
//   --allow-env grants: held.
// - proc.signal without --allow-run: Landlock's scope (ABI 6) or, before
//   it, seccomp lets signals reach this process only: held.

import { CELLS } from "./probes.ts";

export type Column = "macos" | "linux1" | "linux2" | "linux4" | "linux6";

/** The platform column of this machine. */
export function columnFor(os: string, abi: number | null): Column {
  if (os === "darwin") return "macos";
  if (abi == null || abi < 2) return "linux1";
  if (abi < 4) return "linux2";
  if (abi < 6) return "linux4";
  return "linux6";
}

const HELD = new Set(["held", "host", "n/a"]);
const EITHER = new Set(["open", "held", "host", "n/a"]);
const GRANTED = new Set(["open", "host", "n/a"]);

const IP_CELLS = new Set([
  "net.connect",
  "net.ipv6",
  "net.host",
  "net.internal",
  "net.listen",
  "net.udp",
  "net.dns",
]);
const NON_IP_NET_CELLS = new Set([
  "net.unix",
  "net.unix-abstract",
  "net.netlink",
  "ipc.bus",
]);

export function expectEnforce(
  cell: string,
  column: Column,
  config: string,
  vector: string,
): Set<string> {
  const target = CELLS[cell]?.target[column];
  const net = config === "net-all";
  const run = config === "run";
  if (IP_CELLS.has(cell)) return net ? GRANTED : HELD;
  if (NON_IP_NET_CELLS.has(cell)) return HELD;
  if (cell === "net.netlink-route") return net ? EITHER : HELD;
  if (cell === "exec.run-list") {
    if (run) return EITHER;
    return vector === "child" ? new Set(["n/a"]) : HELD;
  }
  if (cell === "fs.meta") return HELD;
  if (cell === "env") return HELD;
  if (cell === "proc.signal" && !run) return HELD;
  if (vector === "child" && !run) return new Set(["n/a"]);
  return target === "kernel" ? HELD : EITHER;
}
