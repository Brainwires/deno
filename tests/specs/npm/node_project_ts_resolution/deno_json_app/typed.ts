import { a } from "./lib/a";
import { b } from "@/b";
import { c } from "./dir";
import { d } from "./lib/d.js";
import { u } from "~utils";
const all: string[] = [a, b, c, d, u];
console.log(all.join(" "));
