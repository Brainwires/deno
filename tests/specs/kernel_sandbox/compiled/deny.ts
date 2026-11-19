// Copyright 2018-2026 the Deno authors. MIT license.
// P3 (d): --deny-read inside --allow-read holds against native code.
import { openRead } from "./native.ts";

openRead("read inside", "inside/a.txt");
openRead("read denied", "inside/secret.txt");
// a link in the expanded folder can't carry a rule to where it leads
openRead("read link to outside", "inside/link");
