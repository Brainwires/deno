// A mode or owner change on a folder that holds a denied path decides who can
// reach or replace the denied path (mode 000 locks it away, a world-writable
// folder lets another user swap it), so it is refused, through a symlink to
// the folder and through an open handle too. Times only touch the folder's
// own entry and are still set.
import {
  chmodSync as nodeChmodSync,
  closeSync,
  fchmodSync,
  fchownSync,
  lchownSync,
  openSync,
  statSync,
} from "node:fs";

function attempt(name: string, fn: () => void) {
  try {
    fn();
    console.log(`${name}: changed (BUG)`);
  } catch (e) {
    console.log(
      `${name}: ${e instanceof Deno.errors.NotCapable ? "denied" : e}`,
    );
  }
}
async function attemptAsync(name: string, fn: () => Promise<void>) {
  try {
    await fn();
    console.log(`${name}: changed (BUG)`);
  } catch (e) {
    console.log(
      `${name}: ${e instanceof Deno.errors.NotCapable ? "denied" : e}`,
    );
  }
}

const { uid, gid } = statSync("base/a");
attempt("Deno.chmodSync parent", () => Deno.chmodSync("base/a", 0o777));
await attemptAsync("Deno.chmod parent", () => Deno.chmod("base/a", 0o777));
attempt("Deno.chmodSync link to parent", () => Deno.chmodSync("base/link", 0));
attempt("Deno.chownSync parent", () => Deno.chownSync("base/a", uid, gid));
await attemptAsync(
  "Deno.chown parent",
  () => Deno.chown("base/a", uid, gid),
);
attempt("node chmodSync parent", () => nodeChmodSync("base/a", 0o777));
attempt("node lchownSync parent", () => lchownSync("base/a", uid, gid));
const fd = openSync("base/a", "r");
attempt("node fchmodSync parent", () => fchmodSync(fd, 0o777));
attempt("node fchownSync parent", () => fchownSync(fd, uid, gid));
closeSync(fd);
console.log("mode kept:", (Deno.statSync("base/a").mode! & 0o777) === 0o755);

// Times on the folder, and modes of entries without a denied path in them,
// still change.
const time = new Date(2001, 1, 1);
Deno.utimeSync("base/a", time, time);
console.log(
  "utime parent:",
  Deno.statSync("base/a").mtime?.getTime() === time.getTime(),
);
Deno.chmodSync("base/a/sibling", 0o600);
Deno.chmodSync("base/other", 0o700);
console.log(
  "other modes changed:",
  (Deno.statSync("base/a/sibling").mode! & 0o777) === 0o600 &&
    (Deno.statSync("base/other").mode! & 0o777) === 0o700,
);
