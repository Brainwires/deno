// Copyright 2018-2026 the Deno authors. MIT license.
// Confined (--allow-write=inside): every write-class attempt on outside.txt.
import { cstr, libc, O_TRUNC, O_WRONLY } from "./native.ts";

const p = cstr("outside.txt");
const fd = libc.symbols.open(p, O_WRONLY | O_TRUNC);
if (fd >= 0) libc.symbols.close(fd);
libc.symbols.truncate(p, 0n);
libc.symbols.chmod(p, 0o777);
libc.symbols.utimes(p, null);
libc.symbols.rename(p, cstr("inside/moved.txt"));
libc.symbols.unlink(p);
