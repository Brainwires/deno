import { expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

// The runner's reads are its own: the test's stay checked. (A Vitest
// project's modules do not see the `Deno` global, as under Node.js.)
// deno-lint-ignore no-explicit-any
const DenoNs = (globalThis as any).Deno;
const NotCapable = DenoNs.errors.NotCapable;
const dataPath = new URL("./data.txt", import.meta.url);

it("denies the test's own reads", async () => {
  expect(() => DenoNs.readTextFileSync(dataPath)).toThrow(NotCapable);
  await expect(DenoNs.readTextFile(dataPath)).rejects.toThrow(NotCapable);
  expect(() => readFileSync(dataPath, "utf8")).toThrow(NotCapable);
});

it("denies the test's own dynamic imports", async () => {
  // a specifier the module graph cannot know
  const specifier = [".", "only_dynamic.ts"].join("/");
  const denied = /Requires read access/;
  await expect(import(specifier)).rejects.toThrow(denied);
  await expect(vi.importActual(specifier)).rejects.toThrow(denied);
});

it("does not hand the runner's reads to the test", () => {
  const internal = DenoNs[DenoNs.internal];
  expect(internal.takeVitestRunnerFs()).toBeUndefined();
  const ops = internal.core.ops;
  expect(ops.op_vitest_runner_take()).toBeNull();
  // without the runner's token, the ops read with the test's permissions
  const path = fileURLToPath(dataPath);
  expect(() => ops.op_vitest_runner_read_text("guess", path)).toThrow(
    NotCapable,
  );
  expect(() => ops.op_vitest_runner_stat("guess", path)).toThrow(NotCapable);
  expect(ops.op_vitest_runner_allow_import("guess", dataPath.href)).toBe(
    false,
  );
});

it("does not let the runner read outside the project", async () => {
  await expect(vi.importMock("../../outside/secret.ts")).rejects.toThrow(
    NotCapable,
  );
});
