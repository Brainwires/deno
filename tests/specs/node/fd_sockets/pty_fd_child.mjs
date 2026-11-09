import tty from "node:tty";
const stream = new tty.ReadStream(3);
console.log("child isTTY", stream.isTTY, "READY");
stream.setEncoding("utf8");
stream.on("data", (data) => {
  if (data.includes("ping")) {
    console.log("child read ping");
    stream.destroy();
  }
});
