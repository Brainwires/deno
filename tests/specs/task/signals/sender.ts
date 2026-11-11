import { signals } from "./signals.ts";

class StdoutReader {
  readonly #reader: ReadableStreamDefaultReader<string>;
  #text = "";

  constructor(stream: ReadableStream<Uint8Array>) {
    const textStream = stream.pipeThrough(new TextDecoderStream());
    this.#reader = textStream.getReader();
  }

  [Symbol.dispose]() {
    this.#reader.releaseLock();
  }

  get text() {
    return this.#text;
  }

  async waitForText(waitingText: string) {
    await this.waitFor((text) => text.includes(waitingText), waitingText);
  }

  async waitFor(found: (text: string) => boolean, description: string) {
    if (found(this.#text)) {
      return;
    }

    while (true) {
      const { value, done } = await this.#reader.read();
      if (value) {
        this.#text += value;
        if (found(this.#text)) {
          break;
        }
      }
      if (done) {
        throw new Error("Did not find text: " + description);
      }
    }
  }
}

const command = new Deno.Command(Deno.execPath(), {
  args: ["task", "listener"],
  stdout: "piped",
});

const child = command.spawn();
const reader = new StdoutReader(child.stdout!);

let listenerPid: number | undefined;
let waitingFor = "Ready";

// Hard timeout: if anything hangs, SIGKILL the child and the listener it
// started (uncatchable) and fail. This prevents the test from hanging for 30m
// waiting on CI timeout, since the listener intercepts all signals including
// SIGTERM. Killing `deno task` alone would leave the listener running, and it
// holds this test's stderr open.
const hardTimeout = setTimeout(() => {
  console.error(`Test timed out waiting for "${waitingFor}", sending SIGKILL`);
  console.error("Listener output so far:", JSON.stringify(reader.text));
  for (
    const kill of [
      () => child.kill("SIGKILL"),
      () => listenerPid !== undefined && Deno.kill(listenerPid, "SIGKILL"),
    ]
  ) {
    try {
      kill();
    } catch {
      // the process may have already exited
    }
  }
  Deno.exit(1);
}, 30_000);

try {
  const readyLine = /Ready (\d+)\n/;
  await reader.waitFor((text) => readyLine.test(text), "Ready <pid>");
  listenerPid = Number(readyLine.exec(reader.text)![1]);

  for (const signal of signals) {
    if (signal === "SIGTERM") {
      continue;
    }
    console.error("Sending", signal);
    waitingFor = "Received " + signal;
    child.kill(signal);
    await reader.waitForText(waitingFor);
  }

  console.error("Sending SIGTERM");
  waitingFor = "the child's exit";
  child.kill("SIGTERM");
  const status = await child.status;
  if (!status.success) {
    console.error("Child exited with code", status.code);
    Deno.exit(1);
  }
} finally {
  clearTimeout(hardTimeout);
}
