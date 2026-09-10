// Developer helper: sign and (optionally) send a request the way the Drydock app will.
// It is the canonical reference for the signing scheme — the C# client must produce the
// exact same string and Base64 HMAC.
//
// Usage:
//   DRYDOCK_HMAC_SECRET=... node scripts/sign.mjs GET /v1/lua/730 [http://localhost:8080] [--send]
//
// Prints the headers (and a ready-to-paste curl), and when --send is passed, performs the
// request and prints the status + a short body preview.

import { createHmac, randomBytes } from "node:crypto";

const [, , methodArg, pathArg, baseArg, ...rest] = process.argv;
const method = (methodArg ?? "GET").toUpperCase();
const pathAndQuery = pathArg ?? "/v1/health";
const base = (baseArg && !baseArg.startsWith("--") ? baseArg : "http://localhost:8080").replace(/\/+$/, "");
const send = process.argv.includes("--send");

const secret = process.env.DRYDOCK_HMAC_SECRET?.split(",")[0]?.trim();
if (!secret) {
  console.error("Set DRYDOCK_HMAC_SECRET (single secret, or comma-separated: the first is used).");
  process.exit(1);
}

const timestamp = String(Math.floor(Date.now() / 1000));
const nonce = randomBytes(16).toString("hex");
const signingString = `${method}\n${pathAndQuery}\n${timestamp}\n${nonce}`;
const signature = createHmac("sha256", secret).update(signingString, "utf8").digest("base64");

const headers = {
  "X-Drydock-Timestamp": timestamp,
  "X-Drydock-Nonce": nonce,
  "X-Drydock-Signature": signature,
};

console.log("Signing string (LF-separated):");
console.log(JSON.stringify(signingString));
console.log("\nHeaders:");
for (const [key, value] of Object.entries(headers)) console.log(`  ${key}: ${value}`);
console.log("\ncurl:");
const headerFlags = Object.entries(headers)
  .map(([k, v]) => `-H '${k}: ${v}'`)
  .join(" ");
console.log(`curl -X ${method} ${headerFlags} '${base}${pathAndQuery}'`);

if (send) {
  const response = await fetch(`${base}${pathAndQuery}`, { method, headers });
  const body = await response.text();
  console.log(`\n--> ${response.status} ${response.statusText}`);
  console.log(body.slice(0, 500));
}
