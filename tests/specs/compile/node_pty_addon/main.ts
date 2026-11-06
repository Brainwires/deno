import pty from "node-pty";
console.log(typeof pty.native, typeof pty.native.fork, pty.native.fork());
