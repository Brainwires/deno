// --env-file and --import-map under enforce: the env file is loaded by the
// image before the sandbox (the confined one doesn't read it again), the
// import map given by flag is readable
import { greet } from "greet";
console.log(Deno.env.get("GREETING"));
console.log(greet);
