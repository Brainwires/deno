#!/usr/bin/env -S deno run --allow-all
// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console

// denext: the flake hunt. Runs the fork's test suites the way
// .github/workflows/denext_tests_os.yml runs them (the same test binaries,
// harness, environment and container profile), N times each, on our own
// machines, and reports every test that failed at least once. The release
// gate (.github/denext-runtime/tag_gate.py) wants a `denext tests` run green
// on its FIRST attempt, so a flake found here costs minutes instead of a
// ~3 h gate restart. See tools/README.md ("flake_hunt.ts") for the usage.
//
// No imports: it runs on whatever Deno a host has (biscuits, the Mac mini).

const USAGE = `\
Usage: deno run -A tools/denext/flake_hunt.ts [options]

Builds the candidate once (debug, as the gate's build and test jobs do), then
runs the selected suites N times each in parallel and reports every test that
failed at least once (the harness's own CI retries count: a test it had to
retry is a flake). Exit 0 when clean, 1 on any flake, 2 on an infrastructure
error. A clean run that covers libs and unit writes the release marker
~/.denext-runners/flake-hunt/<sha>.ok on the Mac mini.

Options:
  --commit <ref>      the candidate (default origin/development-denext)
  --mode fast|full    fast (default): the suites the train's commits touch
                      (filtered to the changed tests and modules) plus the
                      known-flaky list; full: every suite in full
  --suite <names>     add whole suites to a fast run (comma separated):
                      libs, libs_core, unit, unit_node, node_compat, specs,
                      integration
  --base <ref>        fast mode's base (default: the newest
                      denext-runtime-v* tag that is an ancestor of the
                      candidate and not on it)
  --reps <n>          repetitions per suite (default 10)
  --flaky-reps <n>    repetitions for the known-flaky list (default 30)
  --host <h>          local (default), or an ssh host (biscuits): run there
                      and wait for it; the marker is written here
  --workers <n>       suite runs at once (default 2 in containers, 1 local
                      on macOS)
  --build-jobs <n>    CARGO_BUILD_JOBS for the build (default 4)
  --work-dir <dir>    where builds and runs live (default ~/flake-hunt)
  --image <img>       the CI runner image (default denext-ci-linux-2204:latest)
  --plan              print what would run, after the build, and exit
  --no-build          reuse the last build of the commit as it is
  --clean             remove the commit's build when the run ends
  --no-marker         never write (or remove) the release marker
  --attach <run-id>   with --host: wait for a run already started there
  --help
`;

// ---------------------------------------------------------------------------
// Configuration

/** Tests that flaked in a gate before. Fast mode always runs them, with
 * --flaky-reps repetitions. `suite` is a suite name, `crate` (libs only) the
 * lib test binary, `filter` the test filter. */
const KNOWN_FLAKY: {
  suite: string;
  crate?: string;
  filter: string;
  why: string;
}[] = [
  {
    suite: "unit",
    filter: "webrtc_media_test",
    why: "async SFrame crypto leaked into the next test (denext.27 gate)",
  },
  {
    suite: "unit",
    filter: "webrtc_test",
    why: "same peer-connection teardown paths as webrtc_media_test",
  },
  {
    suite: "libs",
    crate: "deno",
    filter: "supply_chain::jail_os_tests",
    why: "a jail race that exposed a real supervisor bug (denext.27 gate)",
  },
  {
    suite: "libs",
    crate: "deno",
    filter: "tools::bundle::esbuild",
    why: "ETXTBSY on the stand-in esbuild binary (denext.27 gate)",
  },
];

/** The harness suites: `cargo test -p <package> --test <suite>`, sharded as
 * the gate's test matrix shards them (denext_tests_os.yml). */
const HARNESS_SUITES: Record<string, { pkg: string; shards: number }> = {
  unit: { pkg: "unit_tests", shards: 1 },
  "unit_node": { pkg: "unit_node_tests", shards: 1 },
  specs: { pkg: "specs_tests", shards: 2 },
  integration: { pkg: "integration_tests", shards: 2 },
  "node_compat": { pkg: "node_compat_tests", shards: 2 },
};
const ALL_SUITES = ["libs", "libs_core", ...Object.keys(HARNESS_SUITES)];

/** The workflow env of denext_tests.yml / denext_tests_os.yml, plus what a
 * GitHub runner sets (CI) and what our Linux pool's run-job.sh adds. */
const CI_ENV: Record<string, string> = {
  CI: "true",
  CARGO_TERM_COLOR: "always",
  CARGO_INCREMENTAL: "0",
  CARGO_PROFILE_DEV_DEBUG: "0",
  RUST_BACKTRACE: "full",
  RUST_LIB_BACKTRACE: "0",
};

/** The gate's container profile on biscuits (denext-internal
 * ops/runners/linux/run-job.sh): memory, pids, seccomp, AppArmor, the job
 * network with IPv6 link-local, the hashy override. The CPU quota is the
 * whole host (os.cpus() must equal navigator.hardwareConcurrency), with
 * --cpu-shares 256: half a runner job's 512, so the runners win under
 * contention. */
const JOB = {
  mem: "3500m",
  memSwap: "7g",
  pids: "4096",
  cpus: 3, // run-job.sh's --cpus: CARGO_BUILD_JOBS and FILE_TEST_RUNNER_PARALLELISM = cpus - 1
  cpuShares: "256",
  seccomp: "/usr/local/libexec/denext-ci/seccomp-job.json",
  apparmor: "denext-ci-job",
  network: "denext-ci",
  ipBase: 200, // 172.30.0.200 + slot; the runner slots use .101-.109
};

const MIN_FREE_DISK_GB = 35;
const MIN_AVAIL_MEM_GB = 4;
const MARKER_DIR = "~/.denext-runners/flake-hunt";
const FAILURE_OUTPUT_LIMIT = 8 * 1024;

// ---------------------------------------------------------------------------
// Arguments

interface Options {
  commit: string;
  mode: "fast" | "full";
  suites: string[];
  base?: string;
  reps: number;
  flakyReps: number;
  host: string;
  workers?: number;
  buildJobs: number;
  workDir: string;
  image: string;
  plan: boolean;
  build: boolean;
  clean: boolean;
  marker: boolean;
  attach?: string;
  runId?: string;
}

function parseArgs(args: string[]): Options {
  const o: Options = {
    commit: "origin/development-denext",
    mode: "fast",
    suites: [],
    reps: 10,
    flakyReps: 30,
    host: "local",
    buildJobs: 4,
    workDir: "~/flake-hunt",
    image: "denext-ci-linux-2204:latest",
    plan: false,
    build: true,
    clean: false,
    marker: true,
  };
  const value = (i: number) => {
    if (i + 1 >= args.length) die(`${args[i]} wants a value`);
    return args[i + 1];
  };
  const int = (s: string, name: string) => {
    const n = Number(s);
    if (!Number.isInteger(n) || n < 1) {
      die(`${name} wants a positive integer, got ${s}`);
    }
    return n;
  };
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    switch (a) {
      case "--commit":
        o.commit = value(i++);
        break;
      case "--mode": {
        const m = value(i++);
        if (m !== "fast" && m !== "full") {
          die(`--mode wants fast or full, got ${m}`);
        }
        o.mode = m;
        break;
      }
      case "--suite":
        for (const s of value(i++).split(",").filter(Boolean)) {
          if (!ALL_SUITES.includes(s)) {
            die(`unknown suite ${s} (${ALL_SUITES.join(", ")})`);
          }
          if (!o.suites.includes(s)) o.suites.push(s);
        }
        break;
      case "--base":
        o.base = value(i++);
        break;
      case "--reps":
        o.reps = int(value(i++), a);
        break;
      case "--flaky-reps":
        o.flakyReps = int(value(i++), a);
        break;
      case "--host":
        o.host = value(i++);
        break;
      case "--workers":
        o.workers = int(value(i++), a);
        break;
      case "--build-jobs":
        o.buildJobs = int(value(i++), a);
        break;
      case "--work-dir":
        o.workDir = value(i++);
        break;
      case "--image":
        o.image = value(i++);
        break;
      case "--plan":
        o.plan = true;
        break;
      case "--no-build":
        o.build = false;
        break;
      case "--clean":
        o.clean = true;
        break;
      case "--no-marker":
        o.marker = false;
        break;
      case "--attach":
        o.attach = value(i++);
        break;
      case "--run-id":
        o.runId = value(i++);
        break; // set by --host for the remote side
      case "--help":
      case "-h":
        console.log(USAGE);
        Deno.exit(0);
        break;
      default:
        die(`unknown argument ${a}\n\n${USAGE}`);
    }
  }
  o.workDir = expandHome(o.workDir);
  return o;
}

// ---------------------------------------------------------------------------
// Small helpers

function die(msg: string, code = 2): never {
  console.error(`flake_hunt: ${msg}`);
  Deno.exit(code);
}

function log(msg: string) {
  console.log(`[${new Date().toISOString().slice(11, 19)}] ${msg}`);
}

function expandHome(p: string): string {
  return p.startsWith("~/") ? `${Deno.env.get("HOME")}/${p.slice(2)}` : p;
}

function shq(s: string): string {
  return /^[A-Za-z0-9_\/.,:=@%+-]+$/.test(s)
    ? s
    : `'${s.replaceAll("'", `'\\''`)}'`;
}

async function run(
  cmd: string[],
  opts: { cwd?: string; allowFail?: boolean; env?: Record<string, string> } =
    {},
): Promise<{ code: number; stdout: string; stderr: string }> {
  const out = await new Deno.Command(cmd[0], {
    args: cmd.slice(1),
    cwd: opts.cwd,
    env: opts.env,
    stdin: "null",
    stdout: "piped",
    stderr: "piped",
  }).output();
  const r = {
    code: out.code,
    stdout: new TextDecoder().decode(out.stdout),
    stderr: new TextDecoder().decode(out.stderr),
  };
  if (r.code !== 0 && !opts.allowFail) {
    throw new Error(
      `${cmd.join(" ")} exited ${r.code}: ${r.stderr.slice(-2000)}`,
    );
  }
  return r;
}

async function exists(p: string): Promise<boolean> {
  try {
    await Deno.lstat(p);
    return true;
  } catch {
    return false;
  }
}

const isLinux = Deno.build.os === "linux";

async function freeDiskGb(path: string): Promise<number> {
  const r = await run(["df", "-Pk", path]);
  const fields = r.stdout.trim().split("\n").pop()!.split(/\s+/);
  return Number(fields[3]) / 1024 / 1024;
}

async function availMemGb(): Promise<number> {
  if (!isLinux) return Infinity;
  const m = (await Deno.readTextFile("/proc/meminfo")).match(
    /MemAvailable:\s+(\d+)/,
  );
  return m ? Number(m[1]) / 1024 / 1024 : Infinity;
}

function sleep(ms: number) {
  return new Promise((r) => setTimeout(r, ms));
}

// ---------------------------------------------------------------------------
// Remote mode: run on another host over ssh, wait, write the marker here.

async function remote(o: Options, argv: string[]) {
  const host = o.host;
  // plain (non-interactive) ssh commands: an interactive login shell never
  // lets the channel close behind a background process
  const ssh = (script: string, allowFail = false) =>
    run(["ssh", "-n", "-o", "BatchMode=yes", host, script], { allowFail });
  let runId = o.attach;
  let sha: string;
  if (!runId) {
    sha = await resolveLocalSha(o.commit);
    if (o.marker) await removeMarker(sha);
    runId = `${new Date().toISOString().replace(/[-:]/g, "").slice(0, 15)}-${
      sha.slice(0, 8)
    }`;
    // the remote side gets the same options with --host local, the resolved
    // commit and the run id
    const pass: string[] = [];
    for (let i = 0; i < argv.length; i++) {
      if (["--host", "--commit", "--attach", "--run-id"].includes(argv[i])) {
        i++;
        continue;
      }
      pass.push(argv[i]);
    }
    pass.push(
      "--host",
      "local",
      "--commit",
      sha,
      "--run-id",
      runId,
      "--no-marker",
    );
    const remoteDir = "~/flake-hunt";
    await ssh(`mkdir -p ${remoteDir}/runs/${runId}`);
    await run([
      "scp",
      "-q",
      "-o",
      "BatchMode=yes",
      import.meta.filename!,
      `${host}:flake-hunt/flake_hunt.ts`,
    ]);
    // deno is on PATH in a login shell there (nvm-style profiles)
    const deno = (await run([
      "ssh",
      "-n",
      "-o",
      "BatchMode=yes",
      host,
      "bash -lic 'command -v deno' 2>/dev/null",
    ])).stdout
      .trim().split("\n").pop()!;
    if (!deno.startsWith("/")) die(`no deno on ${host}`);
    await ssh(
      `cd ${remoteDir} && (setsid nohup ${deno} run -A flake_hunt.ts ${
        pass.map(shq).join(" ")
      } ` +
        `>runs/${runId}/driver.log 2>&1 </dev/null &) && echo started`,
    );
    log(
      `started ${runId} on ${host} (re-attach: --host ${host} --attach ${runId})`,
    );
  }
  let offset = 0;
  let quietPolls = 0;
  while (true) {
    await sleep(30_000);
    const r = await ssh(
      `cd ~/flake-hunt/runs/${runId} && tail -c +${
        offset + 1
      } driver.log; echo; echo "@@FH-STATUS"; cat status.json 2>/dev/null`,
      true,
    );
    if (r.code !== 0) {
      if (++quietPolls > 20) {
        die(`lost ${host} for 10 minutes; re-attach with --attach ${runId}`);
      }
      continue;
    }
    quietPolls = 0;
    const [chunk, status] = r.stdout.split("\n@@FH-STATUS\n");
    const text = chunk.endsWith("\n") ? chunk.slice(0, -1) : chunk;
    if (text) {
      Deno.stdout.writeSync(new TextEncoder().encode(text));
      offset += new TextEncoder().encode(text).length;
    }
    let st: Status | undefined;
    try {
      st = JSON.parse(status);
    } catch { /* not written yet */ }
    if (st?.state !== "done") continue;
    if (st.exit === 0 && st.qualifies && o.marker) {
      await writeMarker(st.marker!);
      log(`marker written: ${MARKER_DIR}/${st.marker!.sha}.ok`);
    } else if (st.exit === 0 && !st.qualifies) {
      log(
        "clean, but this selection does not qualify for the release marker (full, or fast + libs + unit)",
      );
    }
    Deno.exit(st.exit ?? 2);
  }
}

async function resolveLocalSha(ref: string): Promise<string> {
  if (/^[0-9a-f]{40}$/.test(ref)) return ref;
  const m = ref.match(/^origin\/(.+)$/);
  if (m) await run(["git", "fetch", "-q", "origin", m[1]], { allowFail: true });
  return (await run(["git", "rev-parse", `${ref}^{commit}`])).stdout.trim();
}

// ---------------------------------------------------------------------------
// The release marker

interface Marker {
  sha: string;
  mode: string;
  suites: string[];
  reps: number;
  flakyReps: number;
  tasks: number;
  hosts: string[];
  base?: string;
  started: string;
  finished: string;
  runId: string;
}

async function removeMarker(sha: string) {
  try {
    await Deno.remove(`${expandHome(MARKER_DIR)}/${sha}.ok`);
    log(`removed a stale marker for ${sha}`);
  } catch { /* none */ }
}

async function writeMarker(m: Marker) {
  const dir = expandHome(MARKER_DIR);
  await Deno.mkdir(dir, { recursive: true });
  await Deno.writeTextFile(
    `${dir}/${m.sha}.ok`,
    JSON.stringify(m, null, 2) + "\n",
  );
}

// ---------------------------------------------------------------------------
// The checkout

interface Paths {
  work: string; // ~/flake-hunt
  mirror: string; // the bare mirror
  commitDir: string; // per-commit: src/ (and the farms, in containers)
  src: string;
  build: string; // cargo's JSON messages
  cache: string; // the cargo home and prebuilt V8, shared across commits
  runDir: string;
}

async function prepareCheckout(
  o: Options,
  runId: string,
): Promise<{ sha: string; p: Paths }> {
  const work = o.workDir;
  await Deno.mkdir(`${work}/runs`, { recursive: true });
  const mirror = `${work}/deno.git`;
  if (!await exists(mirror)) {
    log("cloning the fork (bare mirror)");
    await run([
      "git",
      "clone",
      "-q",
      "--bare",
      "https://github.com/Brainwires/deno.git",
      mirror,
    ]);
  }
  await run([
    "git",
    "-C",
    mirror,
    "fetch",
    "-q",
    "--prune",
    "origin",
    "+refs/heads/*:refs/remotes/origin/*",
    "+refs/tags/denext-runtime-v*:refs/tags/denext-runtime-v*",
  ]);
  if (/^[0-9a-f]{40}$/.test(o.commit)) {
    await run(["git", "-C", mirror, "fetch", "-q", "origin", o.commit], {
      allowFail: true,
    });
  }
  const sha =
    (await run(["git", "-C", mirror, "rev-parse", `${o.commit}^{commit}`]))
      .stdout.trim();
  const commitDir = `${work}/c/${sha.slice(0, 12)}`;
  const p: Paths = {
    work,
    mirror,
    commitDir,
    src: `${commitDir}/src`,
    build: `${commitDir}/build`,
    cache: `${work}/cache`,
    runDir: `${work}/runs/${runId}`,
  };
  await Deno.mkdir(p.runDir, { recursive: true });
  const fresh = !await exists(`${p.src}/.git`);
  if (fresh) {
    // one cached build at a time: other commits' builds go first
    await Deno.mkdir(`${work}/c`, { recursive: true });
    for await (const e of Deno.readDir(`${work}/c`)) {
      if (e.name !== sha.slice(0, 12)) await removeTree(`${work}/c/${e.name}`);
    }
    log(`checking out ${sha}`);
    await Deno.mkdir(commitDir, { recursive: true });
    await run(["git", "clone", "-q", "--no-checkout", mirror, p.src]);
    await run([
      "git",
      "-C",
      p.src,
      "remote",
      "set-url",
      "origin",
      "https://github.com/Brainwires/deno.git",
    ]);
    await run(["git", "-C", p.src, "checkout", "-q", sha]);
    await run([
      "git",
      "-C",
      p.src,
      "submodule",
      "update",
      "--init",
      "--depth=1",
      "--",
      "tests/util/std",
      "tests/node_compat/runner/suite",
    ]);
  }
  const freshCache = !await exists(`${p.cache}/v8`);
  await Deno.mkdir(p.build, { recursive: true }).catch(() => {});
  await Deno.mkdir(`${p.cache}/cargo-home`, { recursive: true }).catch(
    () => {},
  );
  await Deno.mkdir(`${p.cache}/v8`, { recursive: true }).catch(() => {});
  if (isLinux && (fresh || freshCache)) {
    // the containers run as the image's runner user (uid 1001), as a job does
    await run(["sudo", "-n", "chown", "-R", "1001:1001", commitDir, p.cache]);
  }
  return { sha, p };
}

async function removeTree(dir: string) {
  if (isLinux) await run(["sudo", "-n", "rm", "-rf", dir], { allowFail: true });
  else await Deno.remove(dir, { recursive: true }).catch(() => {});
}

// ---------------------------------------------------------------------------
// The gate's commands, read from the workflow so they stay in step with it

interface CargoStep {
  name: string; // libs, libs_core, libs_core_snapshot
  suite: string;
  cargoArgs: string[]; // after `cargo test`, before `--`
  testArgs: string[]; // after `--`
}

function parseWorkflow(src: string): CargoStep[] {
  const yml = Deno.readTextFileSync(
    `${src}/.github/workflows/denext_tests_os.yml`,
  );
  const step = (name: string): string => {
    const i = yml.indexOf(`- name: ${name}\n`);
    if (i < 0) die(`no "${name}" step in denext_tests_os.yml`);
    const rest = yml.slice(i).split("\n").slice(1);
    const runIdx = rest.findIndex((l) => /^\s+run:/.test(l));
    const line = rest[runIdx];
    if (/run:\s*>-\s*$/.test(line)) {
      const indent = line.match(/^(\s*)/)![1].length;
      const body: string[] = [];
      for (const l of rest.slice(runIdx + 1)) {
        if (l.trim() === "" || l.match(/^(\s*)/)![1].length <= indent) break;
        body.push(l.trim());
      }
      return body.join(" ");
    }
    return line.replace(/^\s+run:\s*/, "");
  };
  const parse = (name: string, suite: string, stepName: string): CargoStep => {
    const cmd = step(stepName).trim();
    if (!cmd.startsWith("cargo test ")) {
      die(`"${stepName}" is not a cargo test step: ${cmd}`);
    }
    const words = cmd.split(/\s+/).slice(2);
    const dd = words.indexOf("--");
    return {
      name,
      suite,
      cargoArgs: dd < 0 ? words : words.slice(0, dd),
      testArgs: dd < 0 ? [] : words.slice(dd + 1),
    };
  };
  return [
    parse("libs", "libs", "Test libs"),
    parse("libs_core", "libs_core", "Test core libs"),
    parse(
      "libs_core_snapshot",
      "libs_core",
      "Test core libs (snapshot-creating tests, one thread)",
    ),
  ];
}

// ---------------------------------------------------------------------------
// Executors: containers on Linux (the gate's image and profile), the host
// itself (under nice) on macOS.

interface Executor {
  kind: "docker" | "local";
  /** The build tree as the task scripts see it. */
  root: string;
  slots: number;
  /** Run a bash script; output to logFile. Resolves to the exit code. */
  exec(
    script: string,
    opts: {
      name: string;
      slot: number;
      logFile: string;
      timeoutMin: number;
      build?: boolean;
      env: Record<string, string>;
    },
  ): Promise<number>;
  killAll(): Promise<void>;
  hosts(): Promise<string[]>;
}

class DockerExecutor implements Executor {
  kind = "docker" as const;
  root = "/fh/src";
  running = new Set<string>();
  constructor(
    public o: Options,
    public p: Paths,
    public runId: string,
    public slots: number,
    public nproc: number,
    public dns: string[],
  ) {}

  args(
    name: string,
    ip: string,
    build: boolean,
    env: Record<string, string>,
  ): string[] {
    const a = [
      "run",
      "--rm",
      "--name",
      name,
      "--label",
      "flake-hunt=1",
      "--label",
      `flake-hunt.run=${this.runId}`,
      "--init",
      "--user",
      "1001:1001",
      "--workdir",
      "/fh",
      "--hostname",
      "denext-ci",
      "--network",
      `name=${JOB.network},ip=${ip},driver-opt=com.docker.network.endpoint.sysctls=net.ipv6.conf.IFNAME.disable_ipv6=0`,
      "--add-host",
      "hashy.deno.deno.net:127.0.0.1",
      ...this.dns.flatMap((d) => ["--dns", d]),
      "--dns-search",
      ".",
      "--pids-limit",
      JOB.pids,
      "--memory",
      build ? "8g" : JOB.mem,
      "--memory-swap",
      build ? "12g" : JOB.memSwap,
      "--cpus",
      String(build ? this.o.buildJobs : this.nproc),
      "--cpu-shares",
      JOB.cpuShares,
      "--cap-drop",
      "ALL",
      "--security-opt",
      "no-new-privileges",
      "--security-opt",
      `seccomp=${JOB.seccomp}`,
      "--security-opt",
      `apparmor=${JOB.apparmor}`,
      "--mount",
      `type=bind,src=${this.p.commitDir},dst=/fh`,
      "--mount",
      `type=bind,src=${this.p.cache},dst=/fh-cache`,
    ];
    for (const [k, v] of Object.entries(env)) a.push("-e", `${k}=${v}`);
    return a;
  }

  async exec(
    script: string,
    o: {
      name: string;
      slot: number;
      logFile: string;
      timeoutMin: number;
      build?: boolean;
      env: Record<string, string>;
    },
  ): Promise<number> {
    const name = `fh-${this.runId}-${o.name}`.replace(/[^A-Za-z0-9_.-]/g, "_")
      .slice(0, 120);
    const ip = `172.30.0.${
      JOB.ipBase - (o.build ? 1 : 0) + (o.build ? 0 : o.slot)
    }`;
    const args = [
      ...this.args(name, ip, !!o.build, o.env),
      this.o.image,
      "bash",
      "-c",
      script,
    ];
    const cmd = ["docker", ...args];
    // the build takes the fork builds' lock, as every local fork build does
    const full = o.build
      ? ["flock", expandHome("~/.fork-cargo.lock"), ...cmd]
      : cmd;
    this.running.add(name);
    try {
      return await spawnLogged(full, o.logFile, o.timeoutMin, async () => {
        await run(["docker", "kill", name], { allowFail: true });
      });
    } finally {
      this.running.delete(name);
    }
  }

  async killAll() {
    const r = await run([
      "docker",
      "ps",
      "-q",
      "--filter",
      `label=flake-hunt.run=${this.runId}`,
    ], { allowFail: true });
    for (const id of r.stdout.split("\n").filter(Boolean)) {
      await run(["docker", "kill", id], { allowFail: true });
    }
  }

  hosts() {
    return Promise.resolve([
      `${Deno.hostname()} (linux ${Deno.build.arch}, ${this.o.image}, ${this.slots} containers)`,
    ]);
  }
}

class LocalExecutor implements Executor {
  kind = "local" as const;
  root: string;
  slots = 1; // the harnesses' test servers bind fixed ports: one suite at a time
  pids = new Set<number>();
  constructor(public o: Options, public p: Paths) {
    this.root = p.src;
  }

  async exec(
    script: string,
    o: {
      name: string;
      slot: number;
      logFile: string;
      timeoutMin: number;
      build?: boolean;
      env: Record<string, string>;
    },
  ): Promise<number> {
    // nice, and a process group of its own, so a timeout or ^C ends every child
    const cmd = [
      "nice",
      "-n",
      "19",
      "perl",
      "-e",
      "setpgrp(0,0); exec @ARGV or die",
      "--",
      "env",
      "-u",
      "CLAUDE_CODE_ENTRYPOINT",
      ...Object.entries(o.env).map(([k, v]) => `${k}=${v}`),
      "bash",
      "-c",
      script,
    ];
    return await spawnLogged(cmd, o.logFile, o.timeoutMin, (pid) => {
      try {
        Deno.kill(-pid, "SIGKILL");
      } catch { /* gone */ }
      return Promise.resolve();
    }, this.pids);
  }

  killAll() {
    for (const pid of this.pids) {
      try {
        Deno.kill(-pid, "SIGKILL");
      } catch { /* gone */ }
    }
    return Promise.resolve();
  }

  hosts() {
    return Promise.resolve([
      `${Deno.hostname()} (${Deno.build.os} ${Deno.build.arch}, local under nice)`,
    ]);
  }
}

async function spawnLogged(
  cmd: string[],
  logFile: string,
  timeoutMin: number,
  onTimeout: (pid: number) => Promise<void>,
  pids?: Set<number>,
): Promise<number> {
  const child = new Deno.Command("bash", {
    args: ["-c", 'exec "$@" >>"$FH_LOG" 2>&1', "_", ...cmd],
    env: { FH_LOG: logFile },
    stdin: "null",
    stdout: "null",
    stderr: "null",
  }).spawn();
  pids?.add(child.pid);
  let timedOut = false;
  const timer = setTimeout(async () => {
    timedOut = true;
    await Deno.writeTextFile(
      logFile,
      `\n=== FH-TIMEOUT after ${timeoutMin} min\n`,
      { append: true },
    );
    await onTimeout(child.pid);
  }, timeoutMin * 60_000);
  const status = await child.status;
  clearTimeout(timer);
  pids?.delete(child.pid);
  return timedOut ? 124 : status.code;
}

async function makeExecutor(
  o: Options,
  p: Paths,
  runId: string,
): Promise<Executor> {
  if (!isLinux) {
    if (o.workers && o.workers > 1) {
      log(
        "local mode on macOS runs one suite at a time (fixed test server ports)",
      );
    }
    return new LocalExecutor(o, p);
  }
  await run(["docker", "image", "inspect", o.image]).catch(() =>
    die(`no image ${o.image} on this host`)
  );
  const nproc = Number((await run(["nproc"])).stdout.trim());
  let dns: string[] = [];
  try {
    dns = [
      ...(await Deno.readTextFile("/run/systemd/resolve/resolv.conf")).matchAll(
        /^nameserver\s+(\d+\.\d+\.\d+\.\d+)/gm,
      ),
    ]
      .map((m) => m[1])
      .filter((ip) =>
        !/^(10|127|169\.254|192\.168|100\.(6[4-9]|[7-9]\d|1[01]\d|12[0-7]))\./
          .test(ip) && !/^172\.(1[6-9]|2\d|3[01])\./.test(ip)
      )
      .slice(0, 3);
  } catch { /* none */ }
  if (dns.length === 0) dns = ["1.1.1.1", "8.8.8.8"];
  return new DockerExecutor(o, p, runId, o.workers ?? 2, nproc, dns);
}

// ---------------------------------------------------------------------------
// The build

const HARNESS_BUILD = Object.entries(HARNESS_SUITES).map(([t, s]) =>
  `-p ${s.pkg} --test ${t}`
);

function buildScript(
  steps: CargoStep[],
  suites: Set<string>,
  root: string,
  buildDir: string,
  cacheDir: string,
): string {
  const cargoJson = (args: string, out: string) =>
    `cargo test ${args} --no-run --message-format=json-render-diagnostics >${buildDir}/${out}.json.tmp && mv ${buildDir}/${out}.json.tmp ${buildDir}/${out}.json`;
  const lines = [
    "set -euo pipefail",
    `cd ${root}`,
    `export RUSTUP_TOOLCHAIN="$(sed -n 's/^channel = "\\(.*\\)"$/\\1/p' rust-toolchain.toml)"`,
    `export CARGO_HOME=${cacheDir}/cargo-home`,
    `mkdir -p ${buildDir}`,
    `host="$(rustc -vV | sed -n 's/^host: //p')"`,
    // seed the prebuilt V8 library only once: a re-seeded library is newer
    // than the v8 crate's build, and everything above it would rebuild
    `seed=; [ -e target/debug/gn_out/obj/librusty_v8.a ] || [ -e target/debug/gn_out/obj/rusty_v8.lib ] || seed="--seed-dir target/debug"`,
    `eval "$(python3 -I .github/denext-runtime/fetch_rusty_v8.py --target "$host" --dest ${cacheDir}/v8 $seed --print-env)"`,
    `cargo metadata --format-version 1 --no-deps >${buildDir}/metadata.json`,
    "echo '=== build: deno, denort, test_server'",
    // the build job's `Build debug`, verbatim
    "cargo build --locked -p deno -p denort -p test_server --bin deno --bin denort --bin test_server --features=deno/panic-trace",
    "echo '=== build: test_ffi'",
    "cargo build --locked -p test_ffi",
  ];
  for (const s of steps) {
    if (!suites.has(s.suite)) continue;
    lines.push(
      `echo '=== build: ${s.name}'`,
      cargoJson(s.cargoArgs.join(" "), s.name),
    );
  }
  for (const [i, t] of Object.keys(HARNESS_SUITES).entries()) {
    if (!suites.has(t)) continue;
    lines.push(`echo '=== build: ${t}'`, cargoJson(HARNESS_BUILD[i], t));
  }
  lines.push("echo '=== build: done'");
  return lines.join("\n");
}

interface TestExe {
  crate: string;
  version: string;
  exe: string; // relative to the root
  manifestDir: string; // relative to the root
}

function readExes(buildDir: string, name: string, root: string): TestExe[] {
  const out: TestExe[] = [];
  for (
    const line of Deno.readTextFileSync(`${buildDir}/${name}.json`).split("\n")
  ) {
    if (!line.startsWith("{")) continue;
    const m = JSON.parse(line);
    if (m.reason !== "compiler-artifact" || !m.executable || !m.profile?.test) {
      continue;
    }
    const id: string = m.package_id; // path+file:///…/cli#deno@3.0.0-denext.27
    const nv = id.match(/#(?:([^@]+)@)?([^@]+)$/);
    const rel = (p: string) =>
      p.startsWith(root + "/") ? p.slice(root.length + 1) : p;
    const manifestDir = rel(m.manifest_path.replace(/\/Cargo\.toml$/, ""));
    out.push({
      crate: m.target.name,
      version: nv?.[2] ?? "0.0.0",
      exe: rel(m.executable),
      manifestDir,
    });
  }
  return out;
}

// ---------------------------------------------------------------------------
// Selection

interface Selection {
  suite: string;
  /** libs / libs_core: the step; harness suites: undefined */
  step?: string;
  crates?: string[]; // libs: only these binaries (undefined: all of the step)
  filter?: string;
  reps: number;
  why: string;
}

interface Metadata {
  packages: {
    name: string;
    manifest_path: string;
    targets: { kind: string[]; name: string; src_path: string }[];
  }[];
}

async function changedFiles(
  mirror: string,
  base: string,
  sha: string,
): Promise<string[]> {
  return (await run([
    "git",
    "-C",
    mirror,
    "diff",
    "--name-only",
    `${base}..${sha}`,
  ])).stdout.split("\n").filter(Boolean);
}

/** The release tag nearest below the candidate: of the denext-runtime-v*
 * tags that are its ancestors (and not on it), the one fewest commits back.
 * (Not a version sort: the fork orders denext.N above rc.N, git does not.) */
async function defaultBase(mirror: string, sha: string): Promise<string> {
  const tags = (await run([
    "git",
    "-C",
    mirror,
    "tag",
    "-l",
    "denext-runtime-v*",
    "--merged",
    sha,
  ])).stdout
    .split("\n").filter(Boolean);
  let best: { tag: string; n: number } | undefined;
  for (const t of tags) {
    const c = (await run(["git", "-C", mirror, "rev-parse", `${t}^{commit}`]))
      .stdout.trim();
    if (c === sha) continue;
    const n = Number(
      (await run(["git", "-C", mirror, "rev-list", "--count", `${c}..${sha}`]))
        .stdout.trim(),
    );
    if (!best || n < best.n) best = { tag: t, n };
  }
  if (!best) die("no denext-runtime-v* tag below the candidate; pass --base");
  return best.tag;
}

/** Fast mode's selection: what the changed files touch. Conservative and
 * heuristic (README): a test file runs its own tests, a Rust source file its
 * module's lib tests, an extension's JS its unit test files. */
function fastSelection(
  files: string[],
  src: string,
  meta: Metadata,
  libsCrates: Set<string>,
  coreCrates: Set<string>,
  reps: number,
): Selection[] {
  const sel: Selection[] = [];
  const add = (s: Selection) => {
    const key = `${s.suite}|${s.step}|${s.crates?.join(",")}|${s.filter}`;
    if (
      !sel.some((x) =>
        `${x.suite}|${x.step}|${x.crates?.join(",")}|${x.filter}` === key
      )
    ) sel.push(s);
  };
  const unitFiles = [...Deno.readDirSync(`${src}/tests/unit`)].map((e) =>
    e.name
  ).filter((n) => n.endsWith("_test.ts"));
  for (const f of files) {
    let m: RegExpMatchArray | null;
    if ((m = f.match(/^tests\/unit\/([^/]+_test)\.ts$/))) {
      add({ suite: "unit", filter: `::${m[1]}`, reps, why: f });
    } else if (
      (m = f.match(/^tests\/unit_node\/(?:.*\/)?([^/]+_test)\.(?:ts|mjs|js)$/))
    ) {
      add({ suite: "unit_node", filter: m[1], reps, why: f });
    } else if (f.startsWith("tests/unit_node/")) {
      add({ suite: "unit_node", reps, why: f });
    } else if (f.startsWith("tests/specs/")) {
      // the nearest directory with a __test__.jsonc
      let dir = f.split("/").slice(0, -1);
      while (
        dir.length > 2 && !existsSync(`${src}/${dir.join("/")}/__test__.jsonc`)
      ) dir = dir.slice(0, -1);
      if (dir.length > 2) {
        add({
          suite: "specs",
          filter: `specs::${dir.slice(2).join("::")}`,
          reps,
          why: f,
        });
      }
    } else if ((m = f.match(/^tests\/integration\/([^/]+)_tests\.rs$/))) {
      add({ suite: "integration", filter: `::${m[1]}::`, reps, why: f });
    } else if (f.startsWith("tests/node_compat/")) {
      add({ suite: "node_compat", reps, why: f });
    } else if ((m = f.match(/^ext\/node\//)) && !f.endsWith(".rs")) {
      add({ suite: "unit_node", reps, why: f });
    } else if ((m = f.match(/^ext\/([^/]+)\/.*\.(?:js|ts|mjs)$/))) {
      const ext = m[1].replace(/-/g, "_");
      for (const u of unitFiles.filter((u) => u.includes(ext))) {
        add({
          suite: "unit",
          filter: `::${u.replace(/\.ts$/, "")}`,
          reps,
          why: f,
        });
      }
    }
    if (f.endsWith(".rs")) {
      // the lib whose source tree holds the file: its module's tests
      let best: { pkg: string; dir: string } | undefined;
      for (const pkg of meta.packages) {
        const lib = pkg.targets.find((t) =>
          t.kind.includes("lib") || t.kind.includes("proc-macro")
        );
        if (!lib) continue;
        const dir = lib.src_path.replace(/\/[^/]+$/, "");
        const abs = `${src}/${f}`;
        if (
          abs.startsWith(dir + "/") && (!best || dir.length > best.dir.length)
        ) best = { pkg: lib.name, dir };
      }
      if (!best) continue;
      const suite = libsCrates.has(best.pkg)
        ? "libs"
        : coreCrates.has(best.pkg)
        ? "libs_core"
        : undefined;
      if (!suite) continue;
      const modPath = `${src}/${f}`.slice(best.dir.length + 1).replace(
        /\.rs$/,
        "",
      ).replace(/(^|\/)(mod|lib|main)$/, "")
        .split("/").filter(Boolean).join("::");
      add({
        suite,
        step: suite === "libs" ? "libs" : "libs_core",
        crates: [best.pkg],
        filter: modPath || undefined,
        reps,
        why: f,
      });
    }
  }
  return sel;
}

function existsSync(p: string): boolean {
  try {
    Deno.lstatSync(p);
    return true;
  } catch {
    return false;
  }
}

// ---------------------------------------------------------------------------
// Tasks

interface ExeRun {
  exe: TestExe;
  args: string[];
}

interface Task {
  id: string;
  suite: string;
  label: string; // libs, unit, specs (1/2), unit webrtc_test …
  rep: number;
  kind: "libtest" | "harness";
  exes: ExeRun[];
  env: Record<string, string>;
  timeoutMin: number;
  resultsName?: string; // harness: target/test_results_<name>.json
}

function buildTasks(
  sels: Selection[],
  steps: CargoStep[],
  exes: Record<string, TestExe[]>,
  testParallelism: number | undefined,
): Task[] {
  // rep-major, so every suite is in flight at once (realistic load)
  const perSel: Task[][] = [];
  for (const s of sels) {
    const list: Task[] = [];
    const base: Record<string, string> = { ...CI_ENV };
    if (testParallelism) {
      base.FILE_TEST_RUNNER_PARALLELISM = String(testParallelism);
    }
    if (s.suite === "libs" || s.suite === "libs_core") {
      const stepNames = s.step
        ? [s.step]
        : steps.filter((x) => x.suite === s.suite).map((x) => x.name);
      if (s.suite === "libs_core" && s.step === "libs_core") {
        stepNames.push("libs_core_snapshot");
      }
      const runs: ExeRun[] = [];
      for (const sn of stepNames) {
        const step = steps.find((x) => x.name === sn)!;
        for (const e of exes[sn] ?? []) {
          if (s.crates && !s.crates.includes(e.crate)) continue;
          // a fast filter replaces the step's own filters but keeps its skips
          // and thread count
          let args = step.testArgs;
          if (s.filter) {
            args = [];
            for (let i = 0; i < step.testArgs.length; i++) {
              const a = step.testArgs[i];
              if (a === "--skip") args.push(a, step.testArgs[++i]);
              else if (a.startsWith("--")) args.push(a);
            }
            args.push(s.filter);
          }
          runs.push({ exe: e, args });
        }
      }
      if (runs.length === 0) continue;
      const label = `${s.suite}${s.crates ? ` ${s.crates.join(",")}` : ""}${
        s.filter ? ` ${s.filter}` : ""
      }`;
      for (let r = 1; r <= s.reps; r++) {
        list.push({
          id: "",
          suite: s.suite,
          label,
          rep: r,
          kind: "libtest",
          exes: runs,
          env: base,
          timeoutMin: 240,
        });
      }
    } else {
      const h = HARNESS_SUITES[s.suite];
      const exe = (exes[s.suite] ?? [])[0];
      if (!exe) continue;
      const shards = s.filter ? 1 : h.shards;
      for (let r = 1; r <= s.reps; r++) {
        for (let sh = 0; sh < shards; sh++) {
          const env = { ...base };
          if (shards > 1) {
            env.CI_SHARD_INDEX = String(sh);
            env.CI_SHARD_TOTAL = String(shards);
          }
          list.push({
            id: "",
            suite: s.suite,
            label: `${s.suite}${shards > 1 ? ` (${sh + 1}/${shards})` : ""}${
              s.filter ? ` ${s.filter}` : ""
            }`,
            rep: r,
            kind: "harness",
            exes: [{ exe, args: s.filter ? [s.filter] : [] }],
            env,
            timeoutMin: 120,
            resultsName: s.suite,
          });
        }
      }
    }
    perSel.push(list);
  }
  const tasks: Task[] = [];
  for (let i = 0; perSel.some((l) => i < l.length); i++) {
    for (const l of perSel) if (i < l.length) tasks.push(l[i]);
  }
  tasks.forEach((t, i) => t.id = `t${String(i + 1).padStart(4, "0")}`);
  return tasks;
}

/** The bash a task runs: a fresh hard-link copy of the checkout (each run
 * starts from a clean tree, as a CI job does, and runs of the same suite never
 * share a directory), then each test binary as `cargo test` runs it (cwd and
 * CARGO_* of its package), then the results and the git-clean check. */
function taskScript(t: Task, root: string, farm: string | undefined): string {
  const w = farm ?? root;
  const L: string[] = ["set -u"];
  if (farm) {
    L.push(
      `rm -rf ${farm}; mkdir -p ${farm}/target/debug`,
      `cd ${root} && for e in * .[!.]*; do [ "$e" = target ] || [ "$e" = '.[!.]*' ] || cp -al "$e" ${farm}/; done`,
      `cp -al ${root}/target/debug/deps ${farm}/target/debug/`,
      `find ${root}/target/debug -maxdepth 1 -type f -exec cp -al {} ${farm}/target/debug/ \\;`,
    );
  }
  L.push(
    `cd ${w}`,
    `export RUSTUP_TOOLCHAIN="$(sed -n 's/^channel = "\\(.*\\)"$/\\1/p' rust-toolchain.toml)"`,
    `export LD_LIBRARY_PATH=${w}/target/debug/deps:${w}/target/debug`,
    `export DYLD_FALLBACK_LIBRARY_PATH=${w}/target/debug/deps:${w}/target/debug`,
    `git status --porcelain --untracked-files=all 2>/dev/null | grep -v '^?? target/' >/tmp/fh-git-before || true`,
    `rc_all=0`,
  );
  for (const r of t.exes) {
    const e = r.exe;
    L.push(
      `echo "=== FH-EXE ${e.crate}"`,
      `( cd ${w}/${e.manifestDir} && CARGO_MANIFEST_DIR=${w}/${e.manifestDir} CARGO_MANIFEST_PATH=${w}/${e.manifestDir}/Cargo.toml ` +
        `CARGO_PKG_NAME=${shq(e.crate)} CARGO_CRATE_NAME=${
          shq(e.crate.replace(/-/g, "_"))
        } CARGO_PKG_VERSION=${shq(e.version)} ` +
        `CARGO_PRIMARY_PACKAGE=1 CARGO="$(command -v cargo || echo cargo)" ${w}/${e.exe} ${
          r.args.map(shq).join(" ")
        } )`,
      `rc=$?; echo "=== FH-EXIT ${e.crate} $rc"; [ $rc = 0 ] || rc_all=$rc`,
    );
  }
  if (t.resultsName) {
    L.push(
      `echo "=== FH-RESULTS-BEGIN"; cat ${w}/target/test_results_${t.resultsName}.json 2>/dev/null; echo; echo "=== FH-RESULTS-END"`,
      `rm -f ${w}/target/test_results_${t.resultsName}.json`,
    );
  }
  // the gate's "Ensure no git changes" (new entries only, for the shared tree)
  L.push(
    `git status --porcelain --untracked-files=all 2>/dev/null | grep -v '^?? target/' >/tmp/fh-git-after || true`,
    `if ! diff -q /tmp/fh-git-before /tmp/fh-git-after >/dev/null; then echo "=== FH-GIT-DIRTY"; diff /tmp/fh-git-before /tmp/fh-git-after | head -50; echo "=== FH-GIT-END"; fi`,
  );
  if (farm) L.push(`cd / && rm -rf ${farm}`);
  L.push(`exit $rc_all`);
  return L.join("\n");
}

// ---------------------------------------------------------------------------
// Results

interface Failure {
  key: string; // <suite>: <test>
  suite: string;
  test: string;
  kind: "failed" | "retried" | "crashed" | "timeout" | "git-dirty";
  attempts: number; // failed attempts in this rep (the harness's retries)
  output: string;
}

interface TaskResult {
  task: Task;
  code: number;
  seconds: number;
  failures: Failure[];
  logFile: string;
}

function cut(s: string): string {
  return s.length > FAILURE_OUTPUT_LIMIT
    ? `[… ${s.length - FAILURE_OUTPUT_LIMIT} bytes cut]\n${
      s.slice(-FAILURE_OUTPUT_LIMIT)
    }`
    : s;
}

// deno-lint-ignore no-control-regex
const ANSI = /\x1b\[[0-9;]*[A-Za-z]/g;

function parseTask(t: Task, code: number, logText: string): Failure[] {
  const text = logText.replace(ANSI, "");
  const out: Failure[] = [];
  const add = (
    test: string,
    kind: Failure["kind"],
    output: string,
    attempts = 1,
  ) =>
    out.push({
      key: `${t.suite}: ${test}`,
      suite: t.suite,
      test,
      kind,
      attempts,
      output: cut(output),
    });
  if (/=== FH-TIMEOUT/.test(text)) {
    add(`${t.label} (whole run)`, "timeout", text.slice(-FAILURE_OUTPUT_LIMIT));
  }
  const git = text.match(/=== FH-GIT-DIRTY\n([\s\S]*?)=== FH-GIT-END/);
  if (git) add(`${t.label}: modified git-tracked files`, "git-dirty", git[1]);
  if (!/^=== FH-EXE /m.test(text)) {
    // nothing ran: the container or the farm failed (an infrastructure error)
    add(
      `${t.label} (did not start, exit ${code})`,
      "crashed",
      text.slice(-FAILURE_OUTPUT_LIMIT),
    );
    return out;
  }
  if (t.kind === "libtest") {
    const sections = text.split(/^=== FH-EXE /m).slice(1);
    for (const sec of sections) {
      const crate = sec.slice(0, sec.indexOf("\n"));
      const exit = sec.match(/^=== FH-EXIT \S+ (\d+)$/m);
      const rc = exit ? Number(exit[1]) : -1;
      const failed = [...sec.matchAll(/^test (\S+) \.\.\. FAILED$/gm)].map((
        m,
      ) => m[1]);
      for (const name of failed) {
        const block = sec.match(
          new RegExp(
            `^---- ${
              escapeRe(name)
            } std(?:out|err) ----\\n([\\s\\S]*?)(?=^---- |^failures:$|^=== FH-)`,
            "m",
          ),
        );
        add(
          `${crate}::${name}`,
          "failed",
          block?.[1] ?? "(no captured output)",
        );
      }
      if (rc !== 0 && failed.length === 0) {
        const last = [...sec.matchAll(/^test (\S+) \.\.\. /gm)].pop()?.[1];
        add(
          `${crate} (test binary exited ${rc}${
            last ? `; last test reported: ${last}` : ""
          })`,
          "crashed",
          sec.slice(-FAILURE_OUTPUT_LIMIT),
        );
      }
    }
    return out;
  }
  // a harness suite: its JSON report (CI=1), which counts the harness's own
  // retries per test (flakyCount)
  const res = text.match(
    /=== FH-RESULTS-BEGIN\n([\s\S]*?)\n=== FH-RESULTS-END/,
  );
  let report: { tests: HarnessResult[] } | undefined;
  try {
    report = res && res[1].trim() ? JSON.parse(res[1].trim()) : undefined;
  } catch { /* torn */ }
  const flakyOutput = (name: string) => {
    const i = text.indexOf(`${name} was flaky on run`);
    if (i < 0) return undefined;
    const rest = text.slice(text.indexOf("\n", i) + 1);
    const end = rest.search(/^(Warning|\*\*\*WARNING\*\*\*) /m);
    return rest.slice(
      0,
      end < 0 ? FAILURE_OUTPUT_LIMIT : Math.min(end, FAILURE_OUTPUT_LIMIT * 2),
    );
  };
  const failOutput = (name: string) => {
    const i = text.lastIndexOf(name);
    return i < 0
      ? text.slice(-FAILURE_OUTPUT_LIMIT)
      : text.slice(Math.max(0, i - 200), i + FAILURE_OUTPUT_LIMIT);
  };
  const walk = (r: HarnessResult) => {
    if (r.failed && !(r.subTests?.length)) {
      add(
        r.name,
        "failed",
        flakyOutput(r.name) ?? failOutput(r.name),
        1 + (r.flakyCount ?? 0),
      );
    } else if ((r.flakyCount ?? 0) > 0 && !r.failed) {
      add(
        r.name,
        "retried",
        flakyOutput(r.name) ?? failOutput(r.name),
        r.flakyCount!,
      );
    } else if (
      r.failed && (r.flakyCount ?? 0) > 0 && r.subTests?.length &&
      !r.subTests.some((s) => s.failed)
    ) {
      add(
        r.name,
        "failed",
        flakyOutput(r.name) ?? failOutput(r.name),
        1 + r.flakyCount!,
      );
    }
    for (const s of r.subTests ?? []) walk(s);
  };
  for (const r of report?.tests ?? []) walk(r);
  if (code !== 0 && !report) {
    add(
      `${t.label} (harness exited ${code} with no report)`,
      "crashed",
      text.slice(-FAILURE_OUTPUT_LIMIT),
    );
  } else if (code !== 0 && out.length === 0) {
    add(
      `${t.label} (harness exited ${code}, no failed test in its report)`,
      "crashed",
      text.slice(-FAILURE_OUTPUT_LIMIT),
    );
  }
  return out;
}

interface HarnessResult {
  name: string;
  failed?: boolean;
  flakyCount?: number;
  subTests?: HarnessResult[];
}

function escapeRe(s: string) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

// ---------------------------------------------------------------------------
// Status (for --host polling) and the report

interface Status {
  state: "building" | "running" | "done";
  sha?: string;
  done?: number;
  total?: number;
  exit?: number;
  qualifies?: boolean;
  marker?: Marker;
}

let statusFile = "";
async function setStatus(s: Status) {
  if (!statusFile) return;
  await Deno.writeTextFile(`${statusFile}.tmp`, JSON.stringify(s));
  await Deno.rename(`${statusFile}.tmp`, statusFile);
}

function fmtDur(s: number) {
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60);
  return h
    ? `${h}h${String(m).padStart(2, "0")}m`
    : `${m}m${String(Math.round(s % 60)).padStart(2, "0")}s`;
}

function report(
  results: TaskResult[],
  runDir: string,
  header: string[],
): { flakes: number; md: string } {
  const by = new Map<
    string,
    {
      f: Failure;
      reps: Set<string>;
      kinds: Map<string, number>;
      attempts: number;
      log: string;
      total: number;
    }
  >();
  const runsPer = new Map<string, number>();
  for (const r of results) {
    runsPer.set(
      `${r.task.suite}|${r.task.label}`,
      (runsPer.get(`${r.task.suite}|${r.task.label}`) ?? 0) + 1,
    );
  }
  for (const r of results) {
    for (const f of r.failures) {
      let e = by.get(f.key);
      if (!e) {
        e = {
          f,
          reps: new Set(),
          kinds: new Map(),
          attempts: 0,
          log: r.logFile,
          total: runsPer.get(`${r.task.suite}|${r.task.label}`) ?? 0,
        };
        by.set(f.key, e);
      }
      e.reps.add(r.task.id);
      e.kinds.set(f.kind, (e.kinds.get(f.kind) ?? 0) + 1);
      e.attempts += f.attempts;
    }
  }
  const md: string[] = ["# Flake hunt", "", ...header, ""];
  const suiteRows = new Map<string, { runs: number; secs: number }>();
  for (const r of results) {
    const s = suiteRows.get(r.task.label) ?? { runs: 0, secs: 0 };
    s.runs++;
    s.secs += r.seconds;
    suiteRows.set(r.task.label, s);
  }
  md.push("| run | repetitions | mean time |", "| --- | ---: | ---: |");
  for (const [l, s] of suiteRows) {
    md.push(`| ${l} | ${s.runs} | ${fmtDur(s.secs / s.runs)} |`);
  }
  md.push("");
  if (by.size === 0) {
    md.push("**No flakes**: every run passed with no retries.");
  } else {
    md.push(
      `**${by.size} flaky test(s)**`,
      "",
      "| test | failed in | how |",
      "| --- | ---: | --- |",
    );
    const sorted = [...by.values()].sort((a, b) => b.reps.size - a.reps.size);
    for (const e of sorted) {
      md.push(
        `| \`${e.f.key}\` | ${e.reps.size} of ${e.total} | ${
          [...e.kinds].map(([k, n]) => `${k} ${n}`).join(", ")
        }; ${e.attempts} failed attempt(s) |`,
      );
    }
    for (const e of sorted) {
      md.push(
        "",
        `## ${e.f.key}`,
        "",
        `First failure (${e.log}):`,
        "",
        "```",
        e.f.output.trimEnd(),
        "```",
      );
    }
  }
  const text = md.join("\n") + "\n";
  Deno.writeTextFileSync(`${runDir}/report.md`, text);
  Deno.writeTextFileSync(
    `${runDir}/report.json`,
    JSON.stringify(
      {
        flakes: [...by.values()].map((e) => ({
          test: e.f.key,
          failedReps: e.reps.size,
          reps: e.total,
          kinds: Object.fromEntries(e.kinds),
          failedAttempts: e.attempts,
          firstLog: e.log,
          firstOutput: e.f.output,
        })),
        runs: results.map((r) => ({
          id: r.task.id,
          label: r.task.label,
          rep: r.task.rep,
          code: r.code,
          seconds: r.seconds,
          failures: r.failures.length,
        })),
      },
      null,
      2,
    ),
  );
  return { flakes: by.size, md: text };
}

// ---------------------------------------------------------------------------
// Main

async function main() {
  const argv = [...Deno.args];
  const o = parseArgs(argv);
  if (o.host !== "local") return await remote(o, argv);
  if (o.attach) die("--attach needs --host");

  const started = new Date();
  const runId = o.runId ??
    `${started.toISOString().replace(/[-:]/g, "").slice(0, 15)}-local`;
  // one hunt per host at a time (the worker addresses and the farms are fixed)
  await Deno.mkdir(o.workDir, { recursive: true });
  const lock = await Deno.open(`${o.workDir}/hunt.lock`, {
    create: true,
    write: true,
  });
  try {
    await lock.lock(true);
  } catch {
    die("could not lock the work directory");
  }
  const { sha, p } = await prepareCheckout(o, runId);
  statusFile = `${p.runDir}/status.json`;
  await setStatus({ state: "building", sha });
  if (o.marker) await removeMarker(sha);
  log(`candidate ${sha} (${o.mode} mode), run ${runId}, logs in ${p.runDir}`);

  const steps = parseWorkflow(p.src);
  const libsCrates = new Set(
    steps[0].cargoArgs.flatMap((a, i, l) => l[i - 1] === "-p" ? [a] : []),
  );
  const coreCrates = new Set(
    steps[1].cargoArgs.flatMap((a, i, l) => l[i - 1] === "-p" ? [a] : []),
  );

  // what to build: full mode everything; fast mode the suites its selection
  // can reach (decided after `cargo metadata`, which the build writes first)
  const ex = await makeExecutor(o, p, runId);
  const sig = async () => {
    log("interrupted: stopping this run's containers / processes");
    await ex.killAll();
    Deno.exit(130);
  };
  Deno.addSignalListener("SIGINT", sig);
  Deno.addSignalListener("SIGTERM", sig);

  const allSuites = new Set(ALL_SUITES);
  let base: string | undefined;
  let files: string[] = [];
  if (o.mode === "fast") {
    base = o.base ?? await defaultBase(p.mirror, sha);
    files = await changedFiles(p.mirror, base, sha);
    log(`fast mode: ${files.length} file(s) changed since ${base}`);
  }
  // the build covers every suite a fast selection can reach from these files;
  // the cheap way to know is to build them all once per commit (the libs
  // build dominates, and the known-flaky list needs it anyway)
  const buildSuites = allSuites;
  const root = ex.root;
  const buildDir = ex.kind === "docker" ? "/fh/build" : p.build;
  const cacheDir = ex.kind === "docker" ? "/fh-cache" : p.cache;
  const buildLog = `${p.runDir}/build.log`;
  if (o.build || !await exists(`${p.build}/libs.json`)) {
    const free = await freeDiskGb(o.workDir);
    if (free < MIN_FREE_DISK_GB + 30) {
      die(
        `${
          free.toFixed(0)
        } GB free: the build needs ~30 GB above the ${MIN_FREE_DISK_GB} GB floor`,
      );
    }
    log(`building (log: ${buildLog})`);
    const t0 = Date.now();
    const watchdog = setInterval(async () => {
      if (await freeDiskGb(o.workDir) < MIN_FREE_DISK_GB) {
        log(`disk below ${MIN_FREE_DISK_GB} GB: stopping the build`);
        await ex.killAll();
      }
    }, 60_000);
    const code = await ex.exec(
      buildScript(steps, buildSuites, root, buildDir, cacheDir),
      {
        name: "build",
        slot: 0,
        logFile: buildLog,
        timeoutMin: 360,
        build: true,
        env: {
          ...CI_ENV,
          CARGO_BUILD_JOBS: String(o.buildJobs),
          CARGO_TERM_COLOR: "never",
        },
      },
    );
    clearInterval(watchdog);
    if (code !== 0) {
      const tail = (await Deno.readTextFile(buildLog)).split("\n").slice(-40)
        .join("\n");
      await setStatus({ state: "done", sha, exit: 2 });
      die(`the build failed (exit ${code}):\n${tail}`);
    }
    log(`built in ${fmtDur((Date.now() - t0) / 1000)}`);
  }

  const hostRoot = p.src;
  const exes: Record<string, TestExe[]> = {};
  for (const s of steps) {
    if (await exists(`${p.build}/${s.name}.json`)) {
      exes[s.name] = readExes(p.build, s.name, root);
    }
  }
  for (const t of Object.keys(HARNESS_SUITES)) {
    if (await exists(`${p.build}/${t}.json`)) {
      exes[t] = readExes(p.build, t, root);
    }
  }
  const meta: Metadata = JSON.parse(
    (await Deno.readTextFile(`${p.build}/metadata.json`)).replaceAll(
      `"${root}/`,
      `"${hostRoot}/`,
    ),
  );

  let sels: Selection[] = [];
  if (o.mode === "full") {
    for (const s of ALL_SUITES) {
      sels.push({ suite: s, reps: o.reps, why: "full mode" });
    }
  } else {
    sels = fastSelection(files, hostRoot, meta, libsCrates, coreCrates, o.reps);
    for (const s of o.suites) {
      sels = sels.filter((x) => x.suite !== s);
      sels.push({ suite: s, reps: o.reps, why: "--suite" });
    }
    for (const k of KNOWN_FLAKY) {
      if (o.suites.includes(k.suite) && o.reps >= o.flakyReps) continue;
      sels.push({
        suite: k.suite,
        step: k.suite === "libs" ? "libs" : undefined,
        crates: k.crate ? [k.crate] : undefined,
        filter: k.filter,
        reps: o.flakyReps,
        why: `known flaky: ${k.why}`,
      });
    }
    // a whole-suite selection covers that suite's filtered ones
    const whole = new Set(
      sels.filter((s) => !s.filter && !s.crates).map((s) => s.suite),
    );
    sels = sels.filter((s) =>
      !(whole.has(s.suite) && (s.filter || s.crates) && s.reps <= o.reps)
    );
  }
  const testParallelism = ex.kind === "docker"
    ? Math.max(1, JOB.cpus - 1)
    : undefined;
  const tasks = buildTasks(sels, steps, exes, testParallelism);
  const ranSuites = [
    ...new Set(sels.filter((s) => !s.filter && !s.crates).map((s) => s.suite)),
  ];
  log(`plan: ${tasks.length} run(s) on ${ex.slots} worker(s)`);
  for (const s of sels) {
    log(
      `  ${s.suite}${s.crates ? ` [${s.crates.join(",")}]` : ""}${
        s.filter ? ` ~ ${s.filter}` : ""
      } x${s.reps}  (${s.why})`,
    );
  }
  if (o.plan) {
    await setStatus({ state: "done", sha, exit: 0, qualifies: false });
    return;
  }

  // run: `slots` at once, each in its own container and hard-link farm; a new
  // run waits while memory or disk is short, and while our runner pool is busy
  // the hunt keeps to fewer workers
  await setStatus({ state: "running", sha, done: 0, total: tasks.length });
  const results: TaskResult[] = [];
  const free = Array.from({ length: ex.slots }, (_, i) => i);
  const inFlight = new Set<Promise<void>>();
  const t0 = Date.now();
  let next = 0;
  const allowed = async () => {
    if (ex.kind !== "docker") return ex.slots;
    const r = await run(
      ["docker", "ps", "-q", "--filter", "label=denext-ci=1"],
      { allowFail: true },
    );
    const runners = r.stdout.split("\n").filter(Boolean).length;
    return Math.max(1, ex.slots - Math.max(0, runners - 1));
  };
  await Deno.mkdir(`${p.runDir}/logs`, { recursive: true });
  while (next < tasks.length || inFlight.size > 0) {
    if (
      next < tasks.length && free.length > 0 && inFlight.size < await allowed()
    ) {
      if (
        await availMemGb() < MIN_AVAIL_MEM_GB ||
        await freeDiskGb(o.workDir) < MIN_FREE_DISK_GB
      ) {
        if (inFlight.size === 0) {
          log("waiting: memory or disk below the floor");
          await sleep(30_000);
        } else await Promise.race(inFlight);
        continue;
      }
      const t = tasks[next++];
      const slot = free.shift()!;
      const logFile = `${p.runDir}/logs/${t.id}-${
        t.label.replace(/[^A-Za-z0-9_.-]+/g, "_").slice(0, 60)
      }-r${t.rep}.log`;
      const farm = ex.kind === "docker" ? `/fh/w${slot}` : undefined;
      const s0 = Date.now();
      const pr = (async () => {
        await Deno.writeTextFile(
          logFile,
          `=== ${t.id} ${t.label} rep ${t.rep} slot ${slot}\n`,
        );
        const code = await ex.exec(taskScript(t, root, farm), {
          name: t.id,
          slot,
          logFile,
          timeoutMin: t.timeoutMin,
          env: t.env,
        });
        const failures = parseTask(t, code, await Deno.readTextFile(logFile));
        const seconds = (Date.now() - s0) / 1000;
        results.push({ task: t, code, seconds, failures, logFile });
        log(
          `${t.id} ${t.label} rep ${t.rep}: ${
            failures.length
              ? `${failures.length} FAILURE(S): ${
                failures.map((f) => f.test).slice(0, 3).join(", ")
              }`
              : "ok"
          } (${fmtDur(seconds)}) [${results.length}/${tasks.length}]`,
        );
        await setStatus({
          state: "running",
          sha,
          done: results.length,
          total: tasks.length,
        });
      })().finally(() => {
        free.push(slot);
        inFlight.delete(pr);
      });
      inFlight.add(pr);
      continue;
    }
    if (inFlight.size > 0) await Promise.race([...inFlight, sleep(30_000)]);
    else await sleep(10_000);
  }
  const finished = new Date();

  const hosts = await ex.hosts();
  const header = [
    `- candidate: \`${sha}\` (${o.mode} mode${base ? `, base ${base}` : ""})`,
    `- host: ${hosts.join("; ")}`,
    `- ${tasks.length} run(s), ${o.reps} repetition(s) per suite, ${o.flakyReps} for the known-flaky list`,
    `- wall time: ${
      fmtDur((finished.getTime() - t0) / 1000)
    } (run phase), started ${started.toISOString()}`,
  ];
  const { flakes, md } = report(results, p.runDir, header);
  console.log("\n" + md.split("\n## ")[0]);
  console.log(`full report: ${p.runDir}/report.md`);

  const exit = flakes > 0 ? 1 : 0;
  const qualifies = o.mode === "full" ||
    (ranSuites.includes("libs") && ranSuites.includes("unit"));
  const marker: Marker = {
    sha,
    mode: o.mode,
    suites: o.mode === "full" ? ALL_SUITES : ["fast", ...ranSuites],
    reps: o.reps,
    flakyReps: o.flakyReps,
    tasks: tasks.length,
    hosts,
    base,
    started: started.toISOString(),
    finished: finished.toISOString(),
    runId,
  };
  if (exit === 0 && qualifies && o.marker) {
    await writeMarker(marker);
    log(`marker written: ${MARKER_DIR}/${sha}.ok`);
  }
  await setStatus({ state: "done", sha, exit, qualifies, marker });
  if (o.clean) await removeTree(p.commitDir);
  Deno.exit(exit);
}

if (import.meta.main) await main();
