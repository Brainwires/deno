// No imports: `globals: true` provides the API.
import { double } from "@lib/math.ts";

declare const __APP_NAME__: string;

describe("config", () => {
  it("ran the setup file", () => {
    expect((globalThis as Record<string, unknown>).setupRan).toBe(true);
  });

  it("applies define and env", () => {
    expect(import.meta.env.VITE_DEFINED).toBe("from-define");
    expect(import.meta.env.VITE_FROM_DOTENV).toBe("dotenv-value");
    expect(import.meta.env.NOT_EXPOSED).toBeUndefined();
    expect(import.meta.env.FROM_TEST_ENV).toBe("yes");
    expect(__APP_NAME__).toBe("demo");
  });

  it("resolves aliases", () => {
    expect(double(21)).toBe(42);
  });

  const obj = { get: () => "real" };
  it("restoreMocks: spies are restored before each test (1)", () => {
    vi.spyOn(obj, "get").mockReturnValue("mocked");
    expect(obj.get()).toBe("mocked");
  });
  it("restoreMocks: spies are restored before each test (2)", () => {
    expect(obj.get()).toBe("real");
  });

  it("uses the config's testTimeout", async () => {
    await new Promise((r) => setTimeout(r, 2000));
  });
});
