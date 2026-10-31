import process from "node:process";

Deno.test("execArgv is empty under deno test", () => {
  console.log(JSON.stringify(process.execArgv));
});
