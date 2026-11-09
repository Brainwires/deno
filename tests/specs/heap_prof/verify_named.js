const profile = JSON.parse(Deno.readTextFileSync("prof/test.heapprofile"));
if (!profile.head) throw new Error("no head");
console.log("prof/test.heapprofile has a head");
