// localStorage under enforce: this program's own origin data folder is
// writable, with no write grant
const before = localStorage.getItem("runs") ?? "0";
localStorage.setItem("runs", String(Number(before) + 1));
console.log("runs:", localStorage.getItem("runs"));
