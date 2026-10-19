// appends a line to this package's results.txt
const fs = require("node:fs");
const path = require("node:path");
fs.appendFileSync(path.join(__dirname, "results.txt"), process.argv[2] + "\n");
