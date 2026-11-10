// Copyright 2018-2026 the Deno authors. MIT license.

// denext: Node's diagnostic report (`process.report`), the JSON document of
// Node's `node_report.cc` (report version 5), built from the runtime's own
// sources: the header from node:os and node:process, the JavaScript stack
// and heap from V8, the resource usage from `process.resourceUsage()`,
// `userLimits` and `sharedObjects` from an op. `nativeStack`, `libuv` and
// `workers` are Node's empty values: the runtime has no native stack walker
// or libuv handle list, and a worker's subreport would need to interrupt
// its isolate.

(function () {
const { core, primordials } = __bootstrap;
const {
  ArrayPrototypeConcat,
  ArrayPrototypeForEach,
  ArrayPrototypeMap,
  ArrayPrototypePush,
  ArrayPrototypeSlice,
  Date,
  DatePrototypeGetDate,
  DatePrototypeGetFullYear,
  DatePrototypeGetHours,
  DatePrototypeGetMinutes,
  DatePrototypeGetMonth,
  DatePrototypeGetSeconds,
  DatePrototypeGetTime,
  DatePrototypeToISOString,
  JSONParse,
  JSONStringify,
  MathRound,
  NumberIsSafeInteger,
  ObjectKeys,
  ObjectPrototypeHasOwnProperty,
  ObjectPrototypePropertyIsEnumerable,
  String,
  StringPrototypePadStart,
  StringPrototypeSplit,
  StringPrototypeToUpperCase,
  StringPrototypeTrim,
  SymbolPrototypeToString,
} = primordials;
const { op_node_report_native_info } = core.ops;

const lazyOs = core.createLazyLoader("node:os");
const lazyFs = core.createLazyLoader("node:fs");
const lazyPath = core.createLazyLoader("node:path");
const lazyV8 = core.createLazyLoader("node:v8");
const lazyProcess = core.createLazyLoader("node:process");
const lazyWorkerThreads = core.createLazyLoader("node:worker_threads");

function errors() {
  return core.loadExtScript("ext:deno_node/internal/errors.ts");
}
function validators() {
  return core.loadExtScript("ext:deno_node/internal/validators.mjs");
}
// The process object, which the bootstrap hands to `initializeReport`
// (node:process is still loading then, so it can't be imported).
let processObject = null;
function proc() {
  return processObject ?? lazyProcess().default;
}

// Node only sets `glibcVersionRuntime` / `glibcVersionCompiler` on Linux
// binaries linked against glibc. On musl Linux and non-Linux platforms the
// fields are absent, and tools such as rollup rely on that to detect the
// libc flavor (denoland/deno#33948). Evaluated inside `getReport` because
// `core.build` isn't populated until startup runs `setBuildInfo` after the
// snapshot is restored.
function addGlibcVersions(header) {
  if (core.build.os === "linux" && core.build.env === "gnu") {
    header.glibcVersionRuntime = "2.38";
    header.glibcVersionCompiler = "2.38";
  }
}

function threadInfo() {
  const { isMainThread, threadId } = lazyWorkerThreads();
  return { isMainThread, threadId: isMainThread ? 0 : threadId };
}

// The CPUs as the report lists them: each `os.cpus()` entry with its times
// flattened (`{ model, speed, user, nice, sys, idle, irq }`).
function reportCpus(os) {
  return ArrayPrototypeMap(os.cpus(), (cpu) => ({
    model: cpu.model,
    speed: cpu.speed,
    user: cpu.times.user,
    nice: cpu.times.nice,
    sys: cpu.times.sys,
    idle: cpu.times.idle,
    irq: cpu.times.irq,
  }));
}

// The network interfaces as the report lists them: one entry per address,
// with the interface's name.
function reportNetworkInterfaces(os) {
  const out = [];
  const interfaces = os.networkInterfaces();
  ArrayPrototypeForEach(ObjectKeys(interfaces), (name) => {
    ArrayPrototypeForEach(interfaces[name], (iface) => {
      const entry = {
        name,
        internal: iface.internal,
        mac: iface.mac,
        address: iface.address,
        netmask: iface.netmask,
        family: iface.family,
      };
      if (iface.family === "IPv6") {
        entry.scopeid = iface.scopeid;
      }
      ArrayPrototypePush(out, entry);
    });
  });
  return out;
}

// `javascriptStack`: the error's first stack line as the message and the
// rest as frames; a thrown primitive is its string form. `errorProperties`
// holds the error's own enumerable properties (other than `stack` and
// `message`) as strings.
function javascriptStack(err) {
  const result = { message: "", stack: undefined, errorProperties: {} };
  if (err === null || (typeof err !== "object" && typeof err !== "function")) {
    result.message = typeof err === "symbol"
      ? SymbolPrototypeToString(err)
      : String(err);
    result.stack = ["Unavailable."];
    return result;
  }
  let stack;
  try {
    stack = err.stack;
  } catch {
    stack = undefined;
  }
  if (typeof stack === "string" && stack !== "") {
    const lines = StringPrototypeSplit(stack, "\n");
    result.message = lines[0];
    const frames = ArrayPrototypeMap(
      ArrayPrototypeSlice(lines, 1),
      (line) => StringPrototypeTrim(line),
    );
    result.stack = frames.length > 0 ? frames : ["Unavailable."];
  } else {
    try {
      result.message = String(err);
    } catch {
      result.message = "Unavailable.";
    }
    result.stack = ["Unavailable."];
  }
  ArrayPrototypeForEach(ObjectKeys(err), (key) => {
    if (key === "stack" || key === "message") return;
    if (!ObjectPrototypePropertyIsEnumerable(err, key)) return;
    let value;
    try {
      value = err[key];
      value = typeof value === "string"
        ? value
        : typeof value === "symbol"
        ? SymbolPrototypeToString(value)
        : (JSONStringify(value) ?? String(value));
    } catch {
      value = "Unavailable.";
    }
    result.errorProperties[key] = value;
  });
  return result;
}

const safeInt = (n) => (NumberIsSafeInteger(n) ? n : MathRound(n) || 0);

// `javascriptHeap`, Node's `PrintGCStatistics`.
function javascriptHeap() {
  const v8 = lazyV8();
  const stats = v8.getHeapStatistics();
  const heapSpaces = {};
  ArrayPrototypeForEach(v8.getHeapSpaceStatistics(), (space) => {
    heapSpaces[space.space_name] = {
      memorySize: safeInt(space.space_size),
      committedMemory: safeInt(space.physical_space_size),
      capacity: safeInt(space.space_used_size + space.space_available_size),
      used: safeInt(space.space_used_size),
      available: safeInt(space.space_available_size),
    };
  });
  return {
    totalMemory: safeInt(stats.total_heap_size),
    executableMemory: safeInt(stats.total_heap_size_executable),
    totalCommittedMemory: safeInt(stats.total_physical_size),
    availableMemory: safeInt(stats.total_available_size),
    totalGlobalHandlesMemory: safeInt(stats.total_global_handles_size),
    usedGlobalHandlesMemory: safeInt(stats.used_global_handles_size),
    usedMemory: safeInt(stats.used_heap_size),
    memoryLimit: safeInt(stats.heap_size_limit),
    mallocedMemory: safeInt(stats.malloced_memory),
    externalMemory: safeInt(stats.external_memory),
    peakMallocedMemory: safeInt(stats.peak_malloced_memory),
    nativeContextCount: safeInt(stats.number_of_native_contexts),
    detachedContextCount: safeInt(stats.number_of_detached_contexts),
    doesZapGarbage: safeInt(stats.does_zap_garbage),
    heapSpaces,
  };
}

// `resourceUsage`, Node's `PrintResourceUsage`.
function resourceUsage(process, os) {
  const usage = process.resourceUsage();
  const user = usage.userCPUTime / 1e6;
  const kernel = usage.systemCPUTime / 1e6;
  const uptime = process.uptime();
  const percent = (n) => (uptime > 0 ? (n / uptime) * 100 : 0);
  const result = {
    free_memory: os.freemem(),
    total_memory: os.totalmem(),
    rss: process.memoryUsage.rss(),
    available_memory: process.availableMemory(),
    userCpuSeconds: user,
    kernelCpuSeconds: kernel,
    cpuConsumptionPercent: percent(user + kernel),
    userCpuConsumptionPercent: percent(user),
    kernelCpuConsumptionPercent: percent(kernel),
    maxRss: usage.maxRSS * 1024,
    pageFaults: {
      IORequired: usage.majorPageFault,
      IONotRequired: usage.minorPageFault,
    },
    fsActivity: {
      reads: usage.fsRead,
      writes: usage.fsWrite,
    },
  };
  const constrained = process.constrainedMemory();
  if (constrained) {
    result.constrained_memory = constrained;
  }
  return result;
}

function environmentVariables(process) {
  const env = {};
  try {
    ArrayPrototypeForEach(ObjectKeys(process.env), (key) => {
      env[key] = String(process.env[key]);
    });
  } catch {
    // Without --allow-env the section is empty.
  }
  return env;
}

function buildReport(event, trigger, filename, err) {
  const process = proc();
  const os = lazyOs();
  const { threadId } = threadInfo();
  const dumpEventTime = new Date();
  const header = {
    reportVersion: 5,
    event,
    trigger,
    filename: filename ?? null,
    dumpEventTime: DatePrototypeToISOString(dumpEventTime),
    dumpEventTimeStamp: String(DatePrototypeGetTime(dumpEventTime)),
    processId: process.pid,
    threadId,
    cwd: process.cwd(),
    commandLine: ArrayPrototypeConcat(
      [process.argv0 || "node"],
      process.execArgv,
      ArrayPrototypeSlice(process.argv, 1),
    ),
    nodejsVersion: process.version,
    wordSize: 64,
    arch: os.arch(),
    platform: os.platform(),
    componentVersions: process.versions,
    release: process.release,
    osName: os.type(),
    osRelease: os.release(),
    osVersion: os.version(),
    osMachine: os.machine(),
    cpus: reportCpus(os),
    host: os.hostname(),
  };
  addGlibcVersions(header);
  if (!report.excludeNetwork) {
    header.networkInterfaces = reportNetworkInterfaces(os);
  }
  const native = JSONParse(op_node_report_native_info());
  const result = {
    header,
    javascriptStack: javascriptStack(err),
    javascriptHeap: javascriptHeap(),
    nativeStack: [],
    resourceUsage: resourceUsage(process, os),
    libuv: [],
    workers: [],
  };
  if (!report.excludeEnv) {
    result.environmentVariables = environmentVariables(process);
  }
  if (native.userLimits !== undefined) {
    result.userLimits = native.userLimits;
  }
  result.sharedObjects = native.sharedObjects;
  return result;
}

function serialize(value) {
  return report.compact
    ? JSONStringify(value) + "\n"
    : JSONStringify(value, null, 2) + "\n";
}

let sequence = 0;

// Node's default file name: `report.<date>.<time>.<pid>.<tid>.<seq>.json`.
function defaultFilename() {
  const now = new Date();
  const pad = (n, len = 2) => StringPrototypePadStart(String(n), len, "0");
  const date = `${DatePrototypeGetFullYear(now)}${
    pad(DatePrototypeGetMonth(now) + 1)
  }${pad(DatePrototypeGetDate(now))}`;
  const time = `${pad(DatePrototypeGetHours(now))}${
    pad(DatePrototypeGetMinutes(now))
  }${pad(DatePrototypeGetSeconds(now))}`;
  sequence++;
  return `report.${date}.${time}.${proc().pid}.${threadInfo().threadId}.${
    pad(sequence, 3)
  }.json`;
}

function writeStderr(text) {
  try {
    lazyFs().writeSync(2, text);
  } catch {
    // Nothing to report to.
  }
}

// Node's `TriggerNodeReport`: write the report to `stdout`, `stderr` or a
// file in the report directory, and return its name.
function triggerReport(event, trigger, file, err) {
  const fs = lazyFs();
  let filename = file;
  if (filename === undefined || filename === "") {
    filename = report.filename || defaultFilename();
  }
  if (filename === "stdout" || filename === "stderr") {
    const text = serialize(buildReport(event, trigger, filename, err));
    fs.writeSync(filename === "stdout" ? 1 : 2, text);
    writeStderr("\nNode.js report completed\n");
    return filename;
  }
  const path = lazyPath();
  const directory = report.directory;
  const fullPath = directory && !path.isAbsolute(filename)
    ? path.join(directory, filename)
    : filename;
  const text = serialize(buildReport(event, trigger, filename, err));
  try {
    fs.writeFileSync(fullPath, text);
  } catch (e) {
    writeStderr(
      `\nFailed to open Node.js report file: ${filename} directory: ${
        directory || "."
      } (errno: ${e?.errno ?? 0})\n`,
    );
    return filename;
  }
  writeStderr(
    `\nWriting Node.js report to file: ${filename}\nNode.js report completed\n`,
  );
  return filename;
}

function writeReport(file, err) {
  if (typeof file === "object" && file !== null) {
    err = file;
    file = undefined;
  } else if (file !== undefined) {
    validators().validateString(file, "file");
  }
  if (err === undefined) {
    err = new (errors().ERR_SYNTHETIC)();
  } else {
    validators().validateObject(err, "err");
  }
  return triggerReport("JavaScript API", "API", file, err);
}

function getReport(err) {
  if (err === undefined) {
    err = new (errors().ERR_SYNTHETIC)();
  } else {
    validators().validateObject(err, "err");
  }
  return buildReport("JavaScript API", "GetReport", null, err);
}

// `--report-uncaught-exception`: called from `process._fatalException`
// when nothing handled the error.
let uncaughtReported = false;
function reportUncaughtException(err) {
  if (!report.reportOnUncaughtException || uncaughtReported) return;
  uncaughtReported = true;
  try {
    triggerReport("Exception", "Exception", undefined, err);
  } catch {
    // A report that fails to build must not hide the original error.
  }
}

// `--report-on-signal`: a listener on `report.signal`, main thread only.
function signalHandler(signal) {
  triggerReport(signal, "Signal", undefined, undefined);
}

let directory = "";
let filename = "";
let compact = false;
let excludeNetwork = false;
let excludeEnv = false;
let reportOnFatalError = false;
let reportOnSignal = false;
let reportOnUncaughtException = false;
let signal = "SIGUSR2";

function validateSignalName(value, name) {
  validators().validateString(value, name);
  const { signals } = lazyOs().constants;
  if (!ObjectPrototypeHasOwnProperty(signals, value)) {
    const { ERR_UNKNOWN_SIGNAL } = errors();
    if (
      ObjectPrototypeHasOwnProperty(signals, StringPrototypeToUpperCase(value))
    ) {
      throw new ERR_UNKNOWN_SIGNAL(
        value + " (signals must use all capital letters)",
      );
    }
    throw new ERR_UNKNOWN_SIGNAL(value);
  }
}

// Set by `initializeReport`: signal reports are the main thread's only.
let onMainThread = true;

function addSignalHandler(sig) {
  if (onMainThread) {
    proc().on(sig, signalHandler);
  }
}
function removeSignalHandler(sig) {
  proc().removeListener(sig, signalHandler);
}

// https://nodejs.org/api/process.html#processreport
const report = {
  writeReport,
  getReport,
  get directory() {
    return directory;
  },
  set directory(dir) {
    validators().validateString(dir, "directory");
    directory = dir;
  },
  get filename() {
    return filename;
  },
  set filename(name) {
    validators().validateString(name, "filename");
    filename = name;
  },
  get compact() {
    return compact;
  },
  set compact(b) {
    validators().validateBoolean(b, "compact");
    compact = b;
  },
  get excludeNetwork() {
    return excludeNetwork;
  },
  set excludeNetwork(b) {
    validators().validateBoolean(b, "excludeNetwork");
    excludeNetwork = b;
  },
  get excludeEnv() {
    return excludeEnv;
  },
  set excludeEnv(b) {
    validators().validateBoolean(b, "excludeEnv");
    excludeEnv = b;
  },
  get signal() {
    return signal;
  },
  set signal(sig) {
    validateSignalName(sig, "signal");
    if (reportOnSignal) {
      removeSignalHandler(signal);
      addSignalHandler(sig);
    }
    signal = sig;
  },
  get reportOnFatalError() {
    return reportOnFatalError;
  },
  set reportOnFatalError(trigger) {
    validators().validateBoolean(trigger, "trigger");
    reportOnFatalError = trigger;
  },
  get reportOnSignal() {
    return reportOnSignal;
  },
  set reportOnSignal(trigger) {
    validators().validateBoolean(trigger, "trigger");
    if (trigger === reportOnSignal) return;
    if (trigger) {
      addSignalHandler(signal);
    } else {
      removeSignalHandler(signal);
    }
    reportOnSignal = trigger;
  },
  get reportOnUncaughtException() {
    return reportOnUncaughtException;
  },
  set reportOnUncaughtException(trigger) {
    validators().validateBoolean(trigger, "trigger");
    reportOnUncaughtException = trigger;
  },
};

// The `--report-*` options (which the node shim passes in NODE_OPTIONS),
// applied by the bootstrap. `getOptionValue` is internal/options.ts's.
function initializeReport(getOptionValue, process, isMainThread) {
  processObject = process;
  onMainThread = isMainThread;
  const dir = getOptionValue("--report-dir");
  if (dir) directory = dir;
  const name = getOptionValue("--report-filename");
  if (name) filename = name;
  compact = !!getOptionValue("--report-compact");
  excludeNetwork = !!getOptionValue("--report-exclude-network");
  excludeEnv = !!getOptionValue("--report-exclude-env");
  reportOnFatalError = !!getOptionValue("--report-on-fatalerror");
  reportOnUncaughtException = !!getOptionValue("--report-uncaught-exception");
  const sig = getOptionValue("--report-signal");
  if (sig) signal = sig;
  if (getOptionValue("--report-on-signal")) {
    report.reportOnSignal = true;
  }
}

return {
  initializeReport,
  report,
  reportUncaughtException,
};
})();
