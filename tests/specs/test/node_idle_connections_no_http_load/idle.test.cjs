// A CommonJS test file: `node:module` is loaded, `node:http` is never used.
Deno.test("first", () => {});
Deno.test("second", () => {});
