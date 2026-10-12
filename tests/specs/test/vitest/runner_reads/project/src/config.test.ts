import { expect, it } from "vitest";
import { greet } from "@/greet.ts";
import { answer } from "~lib/answer.ts";

it("applies the config's aliases", () => {
  expect(greet("you")).toBe("hello you");
  expect(answer).toBe(42);
});

it("runs the setup files", () => {
  expect((globalThis as Record<string, unknown>).setupRan).toBe(true);
});

it("loads the .env files", () => {
  expect(import.meta.env.VITE_FROM_DOTENV).toBe("from-dotenv");
  expect(import.meta.env.SECRET_NOT_EXPOSED).toBeUndefined();
});

it("checks the config's own reads", () => {
  expect(import.meta.env.CONFIG_READ).toBe("NotCapable");
});
