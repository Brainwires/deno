import { afterAll, beforeAll, describe, expect, test as base } from "vitest";

const test = base.extend<{ shared: Deno.Listener }>({
  shared: [
    async ({}, use) => {
      const listener = Deno.listen({ hostname: "127.0.0.1", port: 0 });
      await use(listener);
      listener.close();
    },
    { scope: "file" },
  ],
});

// What a suite's hooks or a file-scoped fixture open outlives the test that
// was running when they opened it: not a leak. The other tests are checked.
describe("a server for the whole suite", () => {
  let server: Deno.Listener;
  beforeAll(() => {
    server = Deno.listen({ hostname: "127.0.0.1", port: 0 });
  });
  afterAll(() => {
    server.close();
  });

  test("first", () => {
    expect(server.addr).toBeTruthy();
  });

  test("leaks a timer", () => {
    setTimeout(() => {}, 10_000);
  });

  test("last", () => {
    expect(server.addr).toBeTruthy();
  });
});

describe("a file-scoped fixture", () => {
  test("sets it up", ({ shared }) => {
    expect(shared.addr).toBeTruthy();
  });

  test("uses it again", ({ shared }) => {
    expect(shared.addr).toBeTruthy();
  });

  test("leaks a timer too", () => {
    setTimeout(() => {}, 10_000);
  });

  test("tears it down", () => {
    expect(1).toBe(1);
  });
});
