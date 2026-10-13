// With a Vitest config, `deno test` type-checks the test modules too (an
// alias, Vite's CSS and `?raw` imports included); this one has a type error
// and runs fine with --no-check.
import { expect, it } from "vitest";
import { sum } from "@/sum";
import "./card.css";
import styles from "./card.module.css";
import note from "./note.txt?raw";

it("sums", () => {
  const total: string = sum(1, 2);
  expect(total).toBe(3);
  expect(styles.card).toBe("card");
  expect(note.trim()).toBe("note");
});
