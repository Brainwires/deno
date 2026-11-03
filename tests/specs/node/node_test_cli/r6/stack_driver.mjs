import { spawnSync } from "node:child_process";
// A failure's stack as Node's runner prints it: the runner's own frames are
// `node:internal/...` ones, and a CommonJS file's frames name it by path.
for (const reporter of ["spec", "tap"]) {
  const { stdout } = spawnSync(process.execPath, [
    "--test",
    `--test-reporter=${reporter}`,
    "test/fails.cjs",
  ], { encoding: "utf8" });
  const frame = stdout.split("\n").find((line) =>
    line.includes("fails.cjs:3:9")
  );
  console.log(reporter, {
    userFrameIsAPath: /TestContext\.<anonymous> \((\/|[A-Za-z]:\\)/.test(frame),
    internalFrames: stdout.includes("node:internal/"),
    extFrames: stdout.includes("ext:"),
  });
}
