// A misbehaving IPC peer: it attaches a descriptor (a pipe's write end) to a
// plain message with its own sendmsg(), so no NODE_HANDLE message claims it,
// then closes its own copy. The pipe's read end sees its end of file only once
// the parent closes the copy it received. See the test of the same
// name in child_process_test.ts.
const linux = Deno.build.os === "linux";
const libc = Deno.dlopen(linux ? "libc.so.6" : "/usr/lib/libSystem.B.dylib", {
  sendmsg: { parameters: ["i32", "pointer", "i32"], result: "isize" },
  pipe: { parameters: ["buffer"], result: "i32" },
  close: { parameters: ["i32"], result: "i32" },
  read: { parameters: ["i32", "buffer", "usize"], result: "isize" },
  poll: {
    parameters: ["buffer", linux ? "usize" : "u32", "i32"],
    result: "i32",
  },
});

// Writes `line` on the channel with `fd` attached (SCM_RIGHTS).
function sendWithFd(channelFd, line, fd) {
  const data = new TextEncoder().encode(line);
  const iov = new BigUint64Array([
    BigInt(Deno.UnsafePointer.value(Deno.UnsafePointer.of(data))),
    BigInt(data.length),
  ]);
  // struct cmsghdr { cmsg_len; cmsg_level; cmsg_type; } + one int:
  // Linux's cmsg_len is a size_t, macOS's a socklen_t.
  const SOL_SOCKET = linux ? 1 : 0xffff;
  const SCM_RIGHTS = 1;
  const header = linux ? 16 : 12;
  const control = new Uint8Array(linux ? 24 : 16);
  const cv = new DataView(control.buffer);
  if (linux) cv.setBigUint64(0, BigInt(header + 4), true);
  else cv.setUint32(0, header + 4, true);
  cv.setInt32(linux ? 8 : 4, SOL_SOCKET, true);
  cv.setInt32(linux ? 12 : 8, SCM_RIGHTS, true);
  cv.setInt32(header, fd, true);
  // struct msghdr: Linux's msg_iovlen and msg_controllen are size_t, macOS's
  // an int and a socklen_t.
  const msg = new Uint8Array(linux ? 56 : 48);
  const mv = new DataView(msg.buffer);
  const ptr = (buf) =>
    BigInt(Deno.UnsafePointer.value(Deno.UnsafePointer.of(buf)));
  mv.setBigUint64(16, ptr(iov), true);
  if (linux) {
    mv.setBigUint64(24, 1n, true);
    mv.setBigUint64(32, ptr(control), true);
    mv.setBigUint64(40, BigInt(control.length), true);
  } else {
    mv.setInt32(24, 1, true);
    mv.setBigUint64(32, ptr(control), true);
    mv.setUint32(40, control.length, true);
  }
  const sent = libc.symbols.sendmsg(channelFd, Deno.UnsafePointer.of(msg), 0);
  if (Number(sent) !== data.length) throw new Error(`sendmsg: ${sent}`);
}

// The write end of a pipe goes to the parent; the read end sees its end of
// file once every copy of the write end is closed.
const fds = new Int32Array(2);
if (libc.symbols.pipe(fds) !== 0) throw new Error("pipe failed");
const [readEnd, writeEnd] = fds;
// The parent puts the channel on fd 3 (`stdio[3]`); the runtime drops
// NODE_CHANNEL_FD from the environment once it opened it, as Node does.
sendWithFd(3, '{"stray":true}\n', writeEnd);
libc.symbols.close(writeEnd);
process.on("disconnect", () => {
  // Wait (blocking) up to 10 s for the end of file.
  const pollfd = new Int32Array([readEnd, 1 /* POLLIN */]);
  const ready = libc.symbols.poll(pollfd, linux ? 1n : 1, 10_000);
  const buf = new Uint8Array(1);
  const eof = ready === 1 && Number(libc.symbols.read(readEnd, buf, 1n)) === 0;
  console.log(eof ? "stray descriptor closed" : "stray descriptor still open");
  process.exit(eof ? 0 : 3);
});
process.send("sent");
