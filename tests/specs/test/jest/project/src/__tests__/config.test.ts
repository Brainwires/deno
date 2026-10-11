// A Jest test file: Jest's globals, no imports from a test framework.
import { square } from "@lib/math";
import styles from "../button.css";
import { log } from "log";
import { reexported } from "../reexports.ts";

declare const __APP__: { name: string };
// deno-lint-ignore no-explicit-any
const g = globalThis as any;

describe("config", () => {
  it("runs setupFiles, then setupFilesAfterEnv", () => {
    expect(g.setupOrder).toEqual(["setupFiles", "setupFilesAfterEnv"]);
    // deno-lint-ignore no-explicit-any
    (expect(4) as any).toBeEven();
  });

  it("maps modules (moduleNameMapper, moduleDirectories)", () => {
    expect(square(3)).toBe(9);
    expect(styles.root).toBe("root-class");
    expect(log()).toBe("logged");
  });

  it("links re-exported types", () => {
    expect(reexported).toBe(true);
  });

  it("compares like Jest's equality", () => {
    expect([1, undefined]).toEqual([1]);
    expect([1, undefined]).not.toStrictEqual([1]);
    const fn = jest.fn();
    fn({ a: 1, b: undefined });
    expect(fn).toHaveBeenCalledWith({ a: 1 });
  });

  it("does not fail a test on an unawaited .resolves", () => {
    expect(Promise.resolve(1)).resolves.toBe(1);
  });

  it("injects the config's globals", () => {
    expect(__APP__).toEqual({ name: "jest" });
  });

  it("uses the config's testTimeout", () => {
    expect(g.__vitest_worker__.config.testTimeout).toBe(1234);
  });

  describe("restoreMocks", () => {
    const target = { f: () => "real" };
    it("spies (1)", () => {
      jest.spyOn(target, "f").mockReturnValue("spy");
      expect(target.f()).toBe("spy");
    });
    it("spies (2)", () => {
      expect(target.f()).toBe("real");
    });
  });

  it("names snapshots like Jest", () => {
    expect(expect.getState().currentTestName).toBe(
      "config names snapshots like Jest",
    );
    expect({ $$typeof: "money", amount: 5 }).toMatchSnapshot();
    expect({ b: [1] }).toMatchSnapshot();
  });
});
