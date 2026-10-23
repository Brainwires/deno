function work(): Promise<void> {
  return Promise.resolve();
}

// A floating promise: only a type-aware rule can tell.
work();

await work();
