// Copyright 2018-2026 the Deno authors. MIT license.
// P3 (a) / P4: native code under --allow-read=inside --allow-write=inside.
import { check, cstr, libc, openRead } from "./native.ts";

openRead("read inside", "inside/a.txt");
openRead("read outside", "outside.txt");
check("create outside", libc.symbols.creat(cstr("created.txt"), 0o600));
check("chmod outside", libc.symbols.chmod(cstr("outside.txt"), 0o600));
check("unlink outside", libc.symbols.unlink(cstr("outside.txt")));
check("create inside", libc.symbols.creat(cstr("inside/new.txt"), 0o600));
