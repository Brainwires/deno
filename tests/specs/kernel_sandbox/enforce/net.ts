// Copyright 2018-2026 the Deno authors. MIT license.
// P3 (c): native code may connect only to the granted port (the test
// server listens on both).
import { tcpConnect } from "./native.ts";

tcpConnect("connect granted port", 4545);
tcpConnect("connect other port", 4546);
