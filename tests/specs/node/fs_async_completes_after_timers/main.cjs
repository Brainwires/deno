// An async node:fs / node:dns call started in a timer callback completes
// through the event loop, like libuv's thread pool: its callback runs after
// every timer that is due in the same timers phase, never in the microtask
// checkpoint between two timer lists.
//
// The calls here fail before they reach a thread (a denied path, a closed
// file descriptor, a denied host), so their ops are ready on the first poll:
// the case where a result was delivered early.
const fs = require("node:fs");
const dns = require("node:dns");

const denied = "denied.txt";
const badFd = 987654;

const calls = {
  "fs.stat": (cb) => fs.stat(denied, cb),
  "fs.lstat": (cb) => fs.lstat(denied, cb),
  "fs.access": (cb) => fs.access(denied, cb),
  "fs.readFile": (cb) => fs.readFile(denied, cb),
  "fs.open": (cb) => fs.open(denied, "r", cb),
  "fs.readdir": (cb) => fs.readdir(denied, cb),
  "fs.readlink": (cb) => fs.readlink(denied, cb),
  "fs.realpath": (cb) => fs.realpath(denied, cb),
  "fs.statfs": (cb) => fs.statfs(denied, cb),
  "fs.opendir": (cb) => fs.opendir(denied, cb),
  "fs.cp": (cb) => fs.cp(denied, "copy.txt", cb),
  "fs.copyFile": (cb) => fs.copyFile(denied, "copy.txt", cb),
  "fs.writeFile": (cb) => fs.writeFile(denied, "x", cb),
  "fs.appendFile": (cb) => fs.appendFile(denied, "x", cb),
  "fs.truncate": (cb) => fs.truncate(denied, 0, cb),
  "fs.mkdir": (cb) => fs.mkdir(denied, cb),
  "fs.rmdir": (cb) => fs.rmdir(denied, cb),
  "fs.rm": (cb) => fs.rm(denied, cb),
  "fs.unlink": (cb) => fs.unlink(denied, cb),
  "fs.rename": (cb) => fs.rename(denied, "renamed.txt", cb),
  "fs.link": (cb) => fs.link(denied, "linked.txt", cb),
  "fs.chmod": (cb) => fs.chmod(denied, 0o644, cb),
  // macOS only, as in Node (no O_SYMLINK elsewhere, so `fs.lchmod` is
  // undefined): run by name, see __test__.jsonc.
  "fs.lchmod": (cb) => fs.lchmod(denied, 0o644, cb),
  "fs.chown": (cb) => fs.chown(denied, 0, 0, cb),
  "fs.lchown": (cb) => fs.lchown(denied, 0, 0, cb),
  "fs.utimes": (cb) => fs.utimes(denied, 0, 0, cb),
  "fs.lutimes": (cb) => fs.lutimes(denied, 0, 0, cb),
  "fs.fstat": (cb) => fs.fstat(badFd, cb),
  "fs.fsync": (cb) => fs.fsync(badFd, cb),
  "fs.fdatasync": (cb) => fs.fdatasync(badFd, cb),
  "fs.ftruncate": (cb) => fs.ftruncate(badFd, 0, cb),
  "fs.futimes": (cb) => fs.futimes(badFd, 0, 0, cb),
  "fs.fchmod": (cb) => fs.fchmod(badFd, 0o644, cb),
  "fs.fchown": (cb) => fs.fchown(badFd, 0, 0, cb),
  "fs.read": (cb) => fs.read(badFd, Buffer.alloc(1), 0, 1, 0, cb),
  "fs.write": (cb) => fs.write(badFd, Buffer.alloc(1), 0, 1, 0, cb),
  "fs.promises.stat": (cb) => fs.promises.stat(denied).then(cb, cb),
  "fs.promises.readFile": (cb) => fs.promises.readFile(denied).then(cb, cb),
  "dns.lookup": (cb) => dns.lookup("denied.invalid", cb),
  "dns.lookupService": (cb) => dns.lookupService("127.0.0.1", 80, cb),
  "dns.resolve4": (cb) => dns.resolve4("denied.invalid", cb),
};

function busy(ms) {
  const end = performance.now() + ms;
  while (performance.now() < end);
}

function check(name) {
  return new Promise((resolve) => {
    let secondTimerRan = false;
    let completed = false;
    setTimeout(() => {
      try {
        calls[name](() => {
          if (completed) return;
          completed = true;
          resolve(
            `${name}: ${secondTimerRan ? "after" : "BEFORE"} the second timer`,
          );
        });
      } catch (err) {
        completed = true;
        resolve(`${name}: threw ${err.code ?? err.name}`);
      }
    }, 1);
    // Another duration, so another timer list: the microtask checkpoint
    // between the two lists is where an early result was delivered.
    setTimeout(() => {
      secondTimerRan = true;
    }, 2);
    // Both timers are due when the timers phase starts.
    busy(10);
  });
}

(async () => {
  const only = process.argv.slice(2);
  const all = Object.keys(calls).filter((name) => name !== "fs.lchmod");
  for (const name of only.length ? only : all) {
    console.log(await check(name));
  }
})();
