import process from "node:process";

// An app hook that throws: --trace-exit must not call it.
Error.prepareStackTrace = () => {
  throw new Error("prepareStackTrace threw");
};
process.exit(3);
