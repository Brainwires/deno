// Copyright 2018-2026 the Deno authors. MIT license.
import { classify, parse } from "./classify.ts";

function eq(actual: unknown, expected: unknown) {
  const a = JSON.stringify(actual), e = JSON.stringify(expected);
  if (a !== e) throw new Error(`expected ${e}, got ${a}`);
}

Deno.test("a refusal the unconfined baseline also saw is host policy", () => {
  // A container seccomp profile refusing io_uring_setup in both passes.
  const r = classify(
    parse("held io_uring_setup: Operation not permitted"),
    parse("held io_uring_setup: Operation not permitted"),
  );
  eq(r.state, "host");
});

Deno.test("a prohibit route refusing the metadata address is host policy", () => {
  const r = classify(
    parse(
      "held connect 169.254.169.254:80 (cloud metadata): Permission denied",
    ),
    parse(
      "held connect 169.254.169.254:80 (cloud metadata): Permission denied",
    ),
  );
  eq(r.state, "host");
});

Deno.test("a baseline the probe itself marked host keeps host", () => {
  const r = classify(
    parse("held PTRACE_ATTACH outside pid: Operation not permitted"),
    parse("host PTRACE_ATTACH: Operation not permitted (Yama ptrace_scope=1)"),
  );
  eq(r.state, "host");
});

Deno.test("a refusal only the confined run saw is held (our sandbox)", () => {
  const r = classify(
    parse("held open outside/secret: Operation not permitted"),
    parse("open read 29 bytes outside the read scope"),
  );
  eq(r.state, "held");
});

Deno.test("held without a baseline stays held", () => {
  eq(classify(parse("held x: Permission denied")).state, "held");
});

Deno.test("held after an n/a baseline stays held", () => {
  // fd.inherited: the baseline has no inherited fd 3.
  const r = classify(
    parse("held read inherited fd 3: Permission denied"),
    parse("n/a no fd 3 in this process (not inherited)"),
  );
  eq(r.state, "held");
});

Deno.test("open, n/a, host and crash pass through unchanged", () => {
  for (const s of ["open a", "n/a b", "host c", "crash d"]) {
    eq(classify(parse(s), parse("held z")), parse(s));
  }
});
