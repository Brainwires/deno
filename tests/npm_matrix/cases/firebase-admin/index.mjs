import { generateKeyPairSync } from "node:crypto"; import admin from "firebase-admin";
const { privateKey } = generateKeyPairSync("rsa", { modulusLength: 2048, privateKeyEncoding: { type: "pkcs8", format: "pem" }, publicKeyEncoding: { type: "spki", format: "pem" } });
admin.initializeApp({ credential: admin.credential.cert({ projectId: "p", clientEmail: "x@p.iam.gserviceaccount.com", privateKey }) });
const tok = await admin.auth().createCustomToken("uid1"); const fs = admin.firestore(); if (!fs) throw 0;
if (tok.split(".").length !== 3) throw new Error("token"); console.log("SMOKE_OK"); process.exit(0);
