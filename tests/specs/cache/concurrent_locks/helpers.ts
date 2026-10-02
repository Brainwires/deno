// Drives concurrent `deno` processes for the cross-process lock tests.

const TIMEOUT_MS = 60_000;

export const denoDir = Deno.env.get("DENO_DIR")!;
export const locksDir = `${denoDir}/locks`;

export class DenoProcess {
  #child: Deno.ChildProcess;
  #stderr = "";
  #stdout = "";
  #done: Promise<Deno.CommandStatus>;

  constructor(args: string[], cwd?: string) {
    this.#child = new Deno.Command(Deno.execPath(), {
      args,
      cwd,
      stdout: "piped",
      stderr: "piped",
      env: { NO_COLOR: "1" },
    }).spawn();
    const stdout = this.#collect(this.#child.stdout, (t) => this.#stdout += t);
    const stderr = this.#collect(this.#child.stderr, (t) => this.#stderr += t);
    this.#done = Promise.all([this.#child.status, stdout, stderr]).then((
      [status],
    ) => status);
  }

  async #collect(
    stream: ReadableStream<Uint8Array>,
    append: (text: string) => void,
  ) {
    for await (const text of stream.pipeThrough(new TextDecoderStream())) {
      append(text);
    }
  }

  get stderr() {
    return this.#stderr;
  }

  get stdout() {
    return this.#stdout;
  }

  /** Waits until the process has written `text` to stderr. */
  async waitForStderr(text: string) {
    await waitUntil(
      () => this.#stderr.includes(text),
      () => `stderr to contain ${JSON.stringify(text)}:\n${this.#stderr}`,
    );
  }

  /** Waits until the process has written `text` to stdout. */
  async waitForStdout(text: string) {
    await waitUntil(
      () => this.#stdout.includes(text),
      () => `stdout to contain ${JSON.stringify(text)}:\n${this.#stdout}`,
    );
  }

  /** Whether the process has exited. */
  async isRunning() {
    const exited = await Promise.race([
      this.#done.then(() => true),
      new Promise<boolean>((resolve) => setTimeout(() => resolve(false), 100)),
    ]);
    return !exited;
  }

  /** Waits for the process to exit and asserts it succeeded. */
  async success() {
    const status = await withTimeout(this.#done, "the process to exit");
    if (!status.success) {
      throw new Error(
        `process failed with ${status.code}\nstdout:\n${this.#stdout}\nstderr:\n${this.#stderr}`,
      );
    }
  }
}

export function deno(...args: string[]) {
  return new DenoProcess(args);
}

/** Starts `deno` in the given directory. */
export function denoIn(cwd: string, ...args: string[]) {
  return new DenoProcess(args, cwd);
}

/** Runs `deno` to completion and returns its stderr. */
export async function runDeno(...args: string[]) {
  return await runDenoIn(undefined, ...args);
}

/** Runs `deno` in the given directory to completion; returns its stderr. */
export async function runDenoIn(cwd: string | undefined, ...args: string[]) {
  const process = new DenoProcess(args, cwd);
  await process.success();
  return process.stderr;
}

export function exists(path: string) {
  try {
    Deno.statSync(path);
    return true;
  } catch (err) {
    if (err instanceof Deno.errors.NotFound) {
      return false;
    }
    throw err;
  }
}

/**
 * Takes a lock on the given lock file the way another `deno` would. Call
 * the returned function to release it.
 */
export function holdLock(path: string, exclusive: boolean): () => void {
  const file = Deno.openSync(path, { read: true, write: true, create: true });
  file.lockSync(exclusive);
  return () => {
    file.unlockSync();
    file.close();
  };
}

/** The artifact lock files a `deno ... -L trace` run reported acquiring. */
export function artifactLockFiles(debugStderr: string): string[] {
  const paths = new Set<string>();
  for (const match of debugStderr.matchAll(/Acquired file lock at (.+?) \(/g)) {
    if (/[\\/]artifacts[\\/]/.test(match[1])) {
      paths.add(match[1]);
    }
  }
  return [...paths];
}

export function assert(condition: unknown, message: string): asserts condition {
  if (!condition) {
    throw new Error(message);
  }
}

async function waitUntil(condition: () => boolean, what: () => string) {
  const start = Date.now();
  while (!condition()) {
    if (Date.now() - start > TIMEOUT_MS) {
      throw new Error(`timed out waiting for ${what()}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}

async function withTimeout<T>(promise: Promise<T>, what: string): Promise<T> {
  let timer: number | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(
      () => reject(new Error(`timed out waiting for ${what}`)),
      TIMEOUT_MS,
    );
  });
  try {
    return await Promise.race([promise, timeout]);
  } finally {
    clearTimeout(timer);
  }
}
