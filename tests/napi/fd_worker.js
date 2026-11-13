// Copyright 2018-2026 the Deno authors. MIT license.

// Loads the test addon in a worker (node:worker_threads or web) and reads
// and closes the pipe it opens through node:fs (fd_test.js).
import { parentPort } from "node:worker_threads";
import { loadTestLibrary } from "./common.js";
import { readAddonPipe } from "./fd_common.js";

const result = readAddonPipe(loadTestLibrary(), "from-worker-addon");
if (parentPort) parentPort.postMessage(result);
else self.postMessage(result);
