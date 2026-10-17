import { createCanvas } from "canvas";
const c = createCanvas(50, 50); const g = c.getContext("2d"); g.fillStyle = "red"; g.fillRect(0, 0, 50, 50);
const b = c.toBuffer("image/png"); const px = g.getImageData(1, 1, 1, 1).data;
if (b[1] !== 0x50 || px[0] !== 255) throw new Error("canvas"); console.log("SMOKE_OK");
