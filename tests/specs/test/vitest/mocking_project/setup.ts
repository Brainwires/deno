// A mock registered by a setup file applies to every test file.
vi.mock("@/lib/clock", () => ({ now: () => "setup-now" }));
