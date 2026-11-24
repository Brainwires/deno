// Copyright 2018-2026 the Deno authors. MIT license.
// A deny ceiling on macOS (-A with --deny-read=secret,box/inner
// --deny-write=held/inner,gone/inner; `gone` doesn't exist): a deny holds
// what its path names, so for native code the denied paths and the folders
// above them stay where they are, and everything beside them is free.
import { check, cstr, libc, openRead } from "./native.ts";

function rename(label: string, from: string, to: string) {
  check(label, libc.symbols.rename(cstr(from), cstr(to)));
}

rename("native rename of a read deny", "secret", "moved");
rename("native rename out of a read deny", "secret/key.txt", "key.txt");
rename("native rename of the folder above a read deny", "box", "box2");
rename("native rename beside a read deny", "box/free.txt", "box/free2.txt");
openRead("native read of the deny", "secret/key.txt");
openRead("native read of the deny beneath a folder", "box/inner/key.txt");
rename("native rename of the folder above a write deny", "held", "held2");
check(
  "native create beside a write deny",
  libc.symbols.creat(cstr("held/beside.txt"), 0o600),
);
check(
  "native create in the write deny",
  libc.symbols.creat(cstr("held/inner/new.txt"), 0o600),
);
check(
  "native mkdir of a missing folder above a write deny",
  libc.symbols.mkdir(cstr("gone"), 0o700),
);
rename(
  "native rename onto a missing folder above a write deny",
  "prep",
  "gone",
);
check(
  "native mkdir beside it",
  libc.symbols.mkdir(cstr("made"), 0o700),
);
