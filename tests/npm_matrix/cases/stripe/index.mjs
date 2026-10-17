import Stripe from "stripe"; const s = new Stripe("sk_test_invalid000");
try { await s.customers.list({ limit: 1 }); process.exit(1); } catch (e) { if (e.type !== "StripeAuthenticationError") { console.error(e); process.exit(1); } }
const sig = s.webhooks.generateTestHeaderString({ payload: "{}", secret: "whsec_x" }); s.webhooks.constructEvent("{}", sig, "whsec_x"); console.log("SMOKE_OK");
