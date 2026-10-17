import nm from "nodemailer"; import net from "node:net";
const t = nm.createTransport({ streamTransport: true, buffer: true }); const i = await t.sendMail({ from: "a@b.c", to: "d@e.f", subject: "s", text: "hi", attachments: [{ filename: "x.txt", content: "att" }] });
if (!String(i.message).includes("Subject: s")) throw 0; console.log("SMOKE_OK");
