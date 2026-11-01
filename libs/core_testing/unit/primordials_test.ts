// Copyright 2018-2026 the Deno authors. MIT license.
import { assert, assertArrayEquals, assertThrows, test } from "checkin:testing";

// deno-lint-ignore no-explicit-any
const { primordials } = (globalThis as any).__bootstrap;
const { SafeMap, SafeSet, SafeWeakMap, SafeWeakSet } = primordials;

// A safe collection built from another collection copies it. Read by index,
// a `Set` (no `length`) never ended the loop: `new SafeSet(set)` hung.
test(function testSafeSetFromSet() {
  const plain = new SafeSet(new Set([1, 2, 3]));
  assertArrayEquals([...plain], [1, 2, 3]);
  const copy = new SafeSet(plain);
  assertArrayEquals([...copy], [1, 2, 3]);
});

test(function testSafeMapFromMap() {
  const copy = new SafeMap(new SafeMap([["a", 1], ["b", 2]]));
  assertArrayEquals([...copy.keys()], ["a", "b"]);
  assert(copy.get("b") === 2);
});

test(function testSafeWeakCollectionsFromCollections() {
  const key = {};
  assert(new SafeWeakSet(new SafeSet([key])).has(key));
  assert(new SafeWeakMap(new SafeMap([[key, 1]])).get(key) === 1);
});

test(function testSafeSetFromArrayLikes() {
  assertArrayEquals([...new SafeSet(new Uint8Array([1, 1, 2]))], [1, 2]);
  assertArrayEquals([...new SafeSet("aba")], ["a", "b"]);
  function* gen() {
    yield 4;
    yield 5;
  }
  assertArrayEquals([...new SafeSet(gen())], [4, 5]);
});

// An array-like is still read by index, so a patched array iterator isn't
// called.
test(function testSafeSetIgnoresPatchedArrayIterator() {
  const proto = Object.getPrototypeOf([][Symbol.iterator]());
  const next = proto.next;
  proto.next = () => {
    throw new Error("patched");
  };
  try {
    assertArrayEquals([...new SafeSet([1, 2]).values()], [1, 2]);
  } finally {
    proto.next = next;
  }
});

test(function testSafeSetFromNonIterable() {
  assertThrows(() => new SafeSet(5 as unknown as number[]), TypeError);
});
