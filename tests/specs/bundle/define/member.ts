declare const process: { env: Record<string, string> };

console.log(process.env.NODE_ENV, import.meta.env.MODE);
