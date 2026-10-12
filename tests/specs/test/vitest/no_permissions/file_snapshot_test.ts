import { expect, test } from "vitest";

// The snapshot file exists, but the test may not read it: it fails, rather
// than passing against no snapshot.
test("file snapshot", () => {
  expect("value").toMatchSnapshot();
});
