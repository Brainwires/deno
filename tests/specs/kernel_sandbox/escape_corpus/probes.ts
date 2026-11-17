// Copyright 2018-2026 the Deno authors. MIT license.
// The escape corpus as data: every probe names the guarantee-matrix cell it
// tests, and every cell says what the kernel layer is meant to hold per
// platform column once enforcement exists. Today (no enforcement) every probe
// is expected to be "open" (or n/a / host-blocked): see corpus.ts --expect.

/** Guarantee-matrix target per column. */
export type Target =
  | "kernel" // held by the kernel layer
  | "coarser" // held, but coarser than the Deno rule (see note)
  | "deno-only" // not kernel-enforced; the Deno check is the only guard
  | "by-design" // allowed on purpose (documented)
  | "n/a";

export interface Cell {
  title: string;
  /** macOS, Linux by Landlock ABI (1, 2-3, 4-5, 6+), Windows small tier. */
  target: {
    macos: Target;
    linux1: Target;
    linux2: Target;
    linux4: Target;
    linux6: Target;
    windows: Target;
  };
  note?: string;
}

const t = (
  macos: Target,
  linux1: Target,
  linux2: Target,
  linux4: Target,
  linux6: Target,
  windows: Target,
) => ({ macos, linux1, linux2, linux4, linux6, windows });

export const CELLS: Record<string, Cell> = {
  "fs.read": {
    title: "read file contents outside the read scope",
    target: t("kernel", "kernel", "kernel", "kernel", "kernel", "deno-only"),
  },
  "fs.write": {
    title: "create, truncate, remove or rename outside the write scope",
    target: t("kernel", "kernel", "kernel", "kernel", "kernel", "coarser"),
    note:
      "Windows: Low integrity, writes only to labelled roots. Linux ABI 1-2: truncate via open(O_TRUNC) is a write right; truncate(2) by path needs ABI 3",
  },
  "fs.meta": {
    title: "change mode, times or xattrs of files outside the write scope",
    target: t(
      "kernel",
      "deno-only",
      "deno-only",
      "deno-only",
      "deno-only",
      "coarser",
    ),
    note:
      "Landlock does not mediate chmod/chown/utime/setxattr; enforce mode refuses the path forms",
  },
  "fs.link": {
    title: "move or link an outside object into the scope",
    target: t("kernel", "kernel", "kernel", "kernel", "kernel", "deno-only"),
    note:
      "Landlock ABI 1 refuses every cross-directory link/rename (REFER arrives in ABI 2)",
  },
  "fs.symlink": {
    title: "reach outside through a symlink in scope, including a swap race",
    target: t("kernel", "kernel", "kernel", "kernel", "kernel", "deno-only"),
  },
  "fs.proc": {
    title: "reach outside through /proc magic links",
    target: t("n/a", "kernel", "kernel", "kernel", "kernel", "n/a"),
  },
  "fd.inherited": {
    title: "use a file descriptor opened before the sandbox applied",
    target: t(
      "by-design",
      "by-design",
      "by-design",
      "by-design",
      "by-design",
      "by-design",
    ),
    note:
      "pre-opened fds are the launcher's capabilities; reopening one for a wider mode through /proc is fs.proc",
  },
  "proc.ptrace": {
    title: "read another process's memory or environment, or attach to it",
    target: t("kernel", "kernel", "kernel", "kernel", "kernel", "deno-only"),
    note:
      "Linux: Landlock ptrace scoping (every ABI) plus seccomp; the seccomp-only fallback leaves /proc/<pid>/mem to Yama",
  },
  "proc.signal": {
    title: "signal a process outside the sandboxed tree",
    target: t(
      "kernel",
      "deno-only",
      "deno-only",
      "deno-only",
      "kernel",
      "coarser",
    ),
    note:
      "Landlock signal scoping needs ABI 6; Windows: job object contains the tree only",
  },
  "exec.run-list": {
    title: "execute a program not on --allow-run (with a run list)",
    target: t(
      "by-design",
      "by-design",
      "by-design",
      "by-design",
      "by-design",
      "by-design",
    ),
    note:
      "with --allow-run=<list> any readable program may run (D3); without any run grant exec is kernel-held",
  },
  "env": {
    title: "read an environment variable --allow-env does not grant",
    target: t(
      "deno-only",
      "deno-only",
      "deno-only",
      "deno-only",
      "deno-only",
      "deno-only",
    ),
  },
  "sys.info": {
    title: "read system information --allow-sys does not grant",
    target: t(
      "deno-only",
      "deno-only",
      "deno-only",
      "deno-only",
      "deno-only",
      "deno-only",
    ),
  },
  "sys.surface": {
    title:
      "reach kernel attack surface (io_uring, clone3 flags, keyrings, bpf)",
    target: t("n/a", "kernel", "kernel", "kernel", "kernel", "n/a"),
  },
  "sys.userns": {
    title: "create user / mount namespaces and mount",
    target: t("n/a", "kernel", "kernel", "kernel", "kernel", "n/a"),
  },
  "net.connect": {
    title: "TCP connect to a port no grant names",
    target: t("kernel", "coarser", "coarser", "kernel", "kernel", "deno-only"),
    note:
      "Linux before ABI 4: network is all-or-nothing in Mode A; host scoping is Mode B everywhere",
  },
  "net.ipv6": {
    title: "IPv6 connect to a port no grant names",
    target: t("kernel", "coarser", "coarser", "kernel", "kernel", "deno-only"),
  },
  "net.host": {
    title: "reach an internal address on a port granted only to a name",
    target: t(
      "coarser",
      "coarser",
      "coarser",
      "coarser",
      "coarser",
      "deno-only",
    ),
    note:
      "kernels scope by port only; exact host scoping needs the broker (Mode B)",
  },
  "net.internal": {
    title: "reach a cloud metadata address",
    target: t(
      "coarser",
      "coarser",
      "coarser",
      "coarser",
      "coarser",
      "deno-only",
    ),
    note:
      "held when no grant names port 80; Mode B refuses internal answers for granted names",
  },
  "net.listen": {
    title: "bind and listen on a port no grant names",
    target: t("kernel", "coarser", "coarser", "kernel", "kernel", "deno-only"),
  },
  "net.udp": {
    title: "send UDP datagrams anywhere",
    target: t(
      "kernel",
      "coarser",
      "coarser",
      "coarser",
      "coarser",
      "deno-only",
    ),
    note:
      "Landlock has no UDP rules: UDP is refused by seccomp when the grant is port-scoped",
  },
  "net.dns": {
    title: "resolve names through a resolver no grant names",
    target: t(
      "coarser",
      "coarser",
      "coarser",
      "coarser",
      "coarser",
      "deno-only",
    ),
    note:
      "macOS: the system resolver socket stays open when any network is granted (a DNS tunnel residual)",
  },
  "net.unix": {
    title: "connect a pathname Unix socket outside the grant",
    target: t(
      "kernel",
      "coarser",
      "coarser",
      "coarser",
      "coarser",
      "deno-only",
    ),
    note:
      "Linux: AF_UNIX refused outright unless a socket grant exists; exact paths need Mode B",
  },
  "net.unix-abstract": {
    title: "connect an abstract Unix socket",
    target: t("n/a", "coarser", "coarser", "coarser", "kernel", "n/a"),
    note:
      "Landlock scopes abstract sockets from ABI 6; before that only the AF_UNIX refusal holds",
  },
  "net.netlink-route": {
    title: "open a netlink route socket (getifaddrs, AI_ADDRCONFIG)",
    target: t("n/a", "by-design", "by-design", "by-design", "by-design", "n/a"),
  },
  "net.netlink": {
    title: "open netlink families beyond route",
    target: t("n/a", "kernel", "kernel", "kernel", "kernel", "n/a"),
  },
  "ipc.bus": {
    title: "reach the desktop session bus",
    target: t("n/a", "coarser", "coarser", "coarser", "coarser", "n/a"),
    note:
      "a session-bus connection can start programs outside the sandbox; refused with AF_UNIX for headless runs, a documented gap for Linux desktop apps",
  },
  "ipc.launch": {
    title: "ask LaunchServices / AppleEvents to start programs",
    target: t("kernel", "n/a", "n/a", "n/a", "n/a", "n/a"),
  },
};

export interface Probe {
  id: string;
  cell: keyof typeof CELLS;
}

export const PROBES: Probe[] = [
  { id: "fs.read", cell: "fs.read" },
  { id: "fs.raw_openat", cell: "fs.read" },
  { id: "fs.create", cell: "fs.write" },
  { id: "fs.truncate", cell: "fs.write" },
  { id: "fs.unlink", cell: "fs.write" },
  { id: "fs.rename", cell: "fs.write" },
  { id: "fs.chmod", cell: "fs.meta" },
  { id: "fs.utimes", cell: "fs.meta" },
  { id: "fs.setxattr", cell: "fs.meta" },
  { id: "fs.hardlink_in", cell: "fs.link" },
  { id: "fs.rename_in", cell: "fs.link" },
  { id: "fs.symlink_out", cell: "fs.symlink" },
  { id: "fs.symlink_race", cell: "fs.symlink" },
  { id: "fs.proc_self_root", cell: "fs.proc" },
  { id: "fd.proc_reopen_write", cell: "fs.proc" },
  { id: "fd.inherited_read", cell: "fd.inherited" },
  { id: "proc.environ", cell: "proc.ptrace" },
  { id: "proc.mem", cell: "proc.ptrace" },
  { id: "proc.ptrace", cell: "proc.ptrace" },
  { id: "proc.signal", cell: "proc.signal" },
  { id: "exec.unlisted", cell: "exec.run-list" },
  { id: "env.read", cell: "env" },
  { id: "sys.info", cell: "sys.info" },
  { id: "sys.io_uring", cell: "sys.surface" },
  { id: "sys.clone3", cell: "sys.surface" },
  { id: "sys.keyctl", cell: "sys.surface" },
  { id: "sys.bpf", cell: "sys.surface" },
  { id: "sys.userns", cell: "sys.userns" },
  { id: "sys.mount", cell: "sys.userns" },
  { id: "net.tcp4", cell: "net.connect" },
  { id: "net.raw_connect", cell: "net.connect" },
  { id: "net.fastopen", cell: "net.connect" },
  { id: "net.tcp6", cell: "net.ipv6" },
  { id: "net.internal_granted_port", cell: "net.host" },
  { id: "net.metadata", cell: "net.internal" },
  { id: "net.bind", cell: "net.listen" },
  { id: "net.udp", cell: "net.udp" },
  { id: "net.dns", cell: "net.dns" },
  { id: "net.unix_path", cell: "net.unix" },
  { id: "ipc.docker_sock", cell: "net.unix" },
  { id: "net.unix_abstract", cell: "net.unix-abstract" },
  { id: "net.netlink_route", cell: "net.netlink-route" },
  { id: "net.netlink_other", cell: "net.netlink" },
  { id: "ipc.session_bus", cell: "ipc.bus" },
  { id: "ipc.launchservices", cell: "ipc.launch" },
  { id: "ipc.appleevents", cell: "ipc.launch" },
];

/** The ways native code reaches the OS under a Deno grant. */
export const VECTORS = ["ffi", "napi", "npm", "child"] as const;
/** `nested` (--expect=enforce only): a `deno run -A` the confined host
 * starts, driving the FFI vector; it inherits the host's grant. */
export type Vector =
  | typeof VECTORS[number]
  | "nested"
  | "nested-env"
  | "nested-closed";
