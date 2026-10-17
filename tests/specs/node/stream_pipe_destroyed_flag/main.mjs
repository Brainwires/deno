import { PassThrough, Readable, Writable } from "node:stream";
import zlib from "node:zlib";

// The legacy idiom: flag the stream destroyed, then end it. The data and the
// EOF are still queued when the destination applies backpressure.
class FdSliceLike extends Readable {
  constructor(buf, chunk) {
    super({ highWaterMark: chunk });
    this.buf = buf;
    this.pos = 0;
    this.chunk = chunk;
  }
  _read() {
    const n = Math.min(this.chunk, this.buf.length - this.pos);
    if (n <= 0) {
      this.destroyed = true;
      this.push(null);
      return;
    }
    const start = this.pos;
    this.pos += n;
    setImmediate(() => this.push(this.buf.subarray(start, start + n)));
  }
}

function slowSink(highWaterMark) {
  let bytes = 0;
  const sink = new Writable({
    highWaterMark,
    write(chunk, _enc, cb) {
      bytes += chunk.length;
      setTimeout(cb, 1);
    },
  });
  return { sink, bytes: () => bytes };
}

function finished(stream, what) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error(`${what}: stalled`)),
      10_000,
    );
    stream.on("finish", () => {
      clearTimeout(timer);
      resolve();
    });
    stream.on("error", reject);
  });
}

// 1. A pipe into a slow writer: every byte arrives and 'finish' fires.
{
  const data = Buffer.alloc(256 * 1024, 7);
  const { sink, bytes } = slowSink(1024);
  new FdSliceLike(data, 4096).pipe(sink);
  await finished(sink, "pipe");
  console.log("pipe", bytes() === data.length);
}

// 2. The EOF alone is pending behind the flag (no data buffered).
{
  const data = Buffer.alloc(4096, 1);
  const { sink, bytes } = slowSink(1);
  new FdSliceLike(data, 4096).pipe(sink);
  await finished(sink, "eof-only");
  console.log("eof-only", bytes() === data.length);
}

// 3. Through zlib (nodejs/node#63992): the final chunk is not dropped.
{
  const raw = Buffer.alloc(384 * 1024);
  for (let i = 0; i < raw.length; i++) raw[i] = (i * 31 + (i >> 7)) & 0xff;
  const compressed = zlib.deflateRawSync(raw);
  const { sink, bytes } = slowSink(1024);
  new FdSliceLike(compressed, 16 * 1024).pipe(zlib.createInflateRaw()).pipe(
    sink,
  );
  await finished(sink, "inflate");
  console.log("inflate", bytes() === raw.length);
}

// 4. A stream really destroyed: resume() and pause() stay no-ops (Node 26).
{
  const r = new PassThrough();
  r.destroy();
  let events = 0;
  r.on("resume", () => events++);
  r.on("pause", () => events++);
  r.resume();
  r.pause();
  await new Promise((resolve) => setImmediate(resolve));
  console.log("destroyed no-op", events === 0, r.isPaused() === false);
}
