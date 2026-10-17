import { a } from "./lib/a";
import { b } from "@/b";
import { c } from "./dir";
import { d } from "./lib/d.js";
import { u } from "~utils";
import { vendored } from "@denotest/esm-basic";
import { add } from "@denotest/add";
console.log(a, b, c, d, u, vendored, add(1, 2));
