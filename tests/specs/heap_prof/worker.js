const parts = [];
for (let i = 0; i < 20000; i++) parts.push({ i, s: "x".repeat(i % 64) });
self.postMessage("worker done");
self.close();
