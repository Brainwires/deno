// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// Windows, the small tier: without --allow-run the job object's process
// limit is 1, so native code can't start a program either (the C runtime's
// system() answers -1 when the process can't be created).
import { crt, cstr } from "./win_native.ts";

const rc = crt.symbols.system(cstr("cmd /c exit 0"));
console.log(`native child: ${rc === 0 ? "started" : "refused"}`);
