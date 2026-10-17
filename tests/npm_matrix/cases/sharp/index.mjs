import sharp from "sharp";
const png = await sharp({ create: { width: 64, height: 64, channels: 3, background: "#f00" } }).png().toBuffer();
const meta = await sharp(png).resize(16).metadata(); const out = await sharp(png).resize(16).jpeg().toBuffer();
if (meta.width !== 64 || out[0] !== 0xff) throw new Error("sharp"); console.log("SMOKE_OK");
