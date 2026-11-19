// Copyright 2018-2026 the Deno authors. MIT license.
// Windows, the small tier: with --allow-write=inside the process runs at
// Low integrity, so native writes outside the grant are refused. Reads are
// not confined on Windows (the guarantee matrix says so).
import { create, openRead, openWrite, remove } from "./win_native.ts";

openRead("read inside", "inside\\a.txt");
openRead("read outside", "outside.txt");
create("create outside", "created.txt");
openWrite("write outside", "outside.txt");
remove("delete outside", "outside.txt");
create("create inside", "inside\\new.txt");
openWrite("write inside", "inside\\a.txt");
