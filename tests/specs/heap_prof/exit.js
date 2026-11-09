const parts = [];
for (let i = 0; i < 20000; i++) parts.push({ i, s: "x".repeat(i % 64) });
Deno.exit(55);
