// UCP + x402 buyer script.
//
// Runs the full end-to-end flow against a merchant-ucp server:
//   1. POST   /ucp/v1/checkout-sessions            (create checkout)
//   2. PUT    /ucp/v1/checkout-sessions/{id}        (attach x402 handler)
//   3. POST   /ucp/v1/checkout-sessions/{id}/complete
//        -> 402, then automatically retried with a signed X-Payment header
//           by wrapFetchWithPayment (from @x402/fetch), which builds and
//           signs the Solana transaction using @x402/svm.
//
// This intentionally reuses the same official client library
// (@x402/svm, @x402/fetch) that the Solana/Kora reference demo uses,
// instead of hand-rolling transaction construction — same approach,
// pointed at our own UCP checkout flow instead of a single protected
// endpoint.
//
// Env vars:
//   MERCHANT_BASE_URL    default http://localhost:3000
//   BUYER_KEYPAIR_PATH   default /run/secrets/buyer-keypair.json
//                         (standard 64-byte Solana keypair JSON array)

import { readFileSync } from "fs";
import { x402Client, wrapFetchWithPayment } from "@x402/fetch";
import { registerExactSvmScheme } from "@x402/svm/exact/client";
import { createKeyPairSignerFromBytes } from "@solana/kit";

const MERCHANT_BASE_URL = process.env.MERCHANT_BASE_URL || "http://localhost:3000";
const BUYER_KEYPAIR_PATH = process.env.BUYER_KEYPAIR_PATH || "/run/secrets/buyer-keypair.json";

async function main() {
    console.log("\n=== UCP + x402 buyer flow ===\n");

    // --- Load buyer keypair -------------------------------------------------
    const secretKeyArray: number[] = JSON.parse(readFileSync(BUYER_KEYPAIR_PATH, "utf-8"));
    const secretKeyBytes = new Uint8Array(secretKeyArray);
    const signer = await createKeyPairSignerFromBytes(secretKeyBytes);
    console.log(`[1/4] Buyer signer loaded: ${signer.address}`);

    const client = new x402Client();
    registerExactSvmScheme(client, { signer });
    const fetchWithPayment = wrapFetchWithPayment(fetch, client);

    // --- Step 1: create checkout ---------------------------------------------
    console.log("\n[2/4] Creating checkout session");
    const createRes = await fetch(`${MERCHANT_BASE_URL}/ucp/v1/checkout-sessions`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
            line_items: [
                {
                    id: "li_1",
                    title: "TS buyer script test item",
                    quantity: 1,
                    unit_price: 1_000_000, // 1 USDC in micro-units
                    currency: "USDC",
                },
            ],
            buyer: { email: "ts-buyer-script@example.com" },
        }),
    });

    if (!createRes.ok) {
        throw new Error(`create checkout failed: ${createRes.status} ${await createRes.text()}`);
    }
    const checkout = await createRes.json();
    console.log(`  -> checkout id: ${checkout.id}`);

    // --- Step 2: attach the x402 payment handler ------------------------------
    console.log("\n[3/4] Attaching x402_solana_devnet handler");
    const updateRes = await fetch(
        `${MERCHANT_BASE_URL}/ucp/v1/checkout-sessions/${checkout.id}`,
        {
            method: "PUT",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ payment_handler_id: "x402_solana_devnet" }),
        },
    );
    if (!updateRes.ok) {
        throw new Error(`attach handler failed: ${updateRes.status} ${await updateRes.text()}`);
    }
    const updated = await updateRes.json();
    console.log(`  -> status: ${updated.status}`);
    if (updated.status !== "ready_for_complete") {
        throw new Error(`unexpected status after attaching handler: ${updated.status}`);
    }

    // --- Step 3: complete checkout, letting @x402/fetch handle the 402 --------
    console.log("\n[4/4] Completing checkout (will pay via x402 if challenged)");
    const completeRes = await fetchWithPayment(
        `${MERCHANT_BASE_URL}/ucp/v1/checkout-sessions/${checkout.id}/complete`,
        { method: "POST" },
    );

    const completeBody = await completeRes.json();
    console.log(`\n  -> HTTP ${completeRes.status}`);
    console.log(JSON.stringify(completeBody, null, 2));

    if (completeRes.status === 200) {
        console.log("\n=== SUCCESS: checkout completed and paid ===");
    } else {
        console.log("\n=== FAILED: checkout did not complete ===");
        process.exit(1);
    }
}

main().catch((err) => {
    console.error("\n=== ERROR ===");
    console.error(err instanceof Error ? err.message : err);
    process.exit(1);
});
