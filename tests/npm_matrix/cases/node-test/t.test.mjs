import { describe, it, mock, before } from "node:test"; import assert from "node:assert/strict";
describe("s", () => { let v; before(() => { v = 1; }); it("a", () => assert.equal(v, 1)); it("mock", () => { const f = mock.fn(() => 2); f(); assert.equal(f.mock.calls.length, 1); }); it.todo("t"); });
