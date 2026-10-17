import { Prisma, PrismaClient } from "npm:@denotest/cjs-reexport-pkg-imports";
import * as ns from "npm:@denotest/cjs-reexport-pkg-imports";

console.log(new PrismaClient().hello());
console.log(Prisma.prismaVersion.client);
console.log(Object.keys(ns).sort().join(","));
