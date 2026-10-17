import { JSDOM } from "jsdom"; const d = new JSDOM("<p class=a>x</p><script>document.body.dataset.r='ran'</script>", { runScripts: "dangerously" });
if (d.window.document.querySelector(".a").textContent !== "x" || d.window.document.body.dataset.r !== "ran") throw new Error("jsdom"); console.log("SMOKE_OK");
