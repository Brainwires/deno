// Finishes loading after setup_after_env.ts would have, were the setup files
// imported at once rather than in order (as Jest runs them).
await new Promise((resolve) => setTimeout(resolve, 100));
(globalThis as Record<string, unknown>).setupOrder = ["setupFiles"];
