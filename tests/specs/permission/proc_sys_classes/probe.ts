const paths = [
  "/proc/cpuinfo",
  "/proc/meminfo",
  "/proc/loadavg",
  "/proc/uptime",
  "/proc/version",
  "/proc/self/status",
  "/proc/thread-self/stat",
  "/proc/1/comm",
  // Control surfaces: --allow-all only.
  "/proc/self/mem",
  "/proc/self/root/etc/hostname",
  "/proc/self/../1/mem",
  "/proc/self/environ",
];
for (const path of paths) {
  try {
    Deno.readFileSync(path);
    console.log(`${path}: read`);
  } catch (e) {
    console.log(`${path}: ${(e as Error).name}`);
  }
}
try {
  Array.from(Deno.readDirSync("/proc")).length;
  console.log("readDir /proc: read");
} catch (e) {
  console.log(`readDir /proc: ${(e as Error).name}`);
}
