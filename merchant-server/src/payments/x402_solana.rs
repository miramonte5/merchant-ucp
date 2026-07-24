//! x402/Solana payment handler.
//!
//! Implements the x402 HTTP 402 payment flow for Solana devnet using
//! the Kora demo facilitator as the settlement backend.
//!
//! Flow:
//!   1. Agent calls complete without X-Payment header
//!      → handler returns 402 with PaymentRequirements
//!   2. Agent constructs a USDC transfer tx, signs it, encodes it
//!   3. Agent retries complete with X-Payment header containing the payload
//!      → handler calls facilitator POST /settle
//!      → facilitator calls Kora signAndSendTransaction
//!      → Kora submits tx to Solana devnet
//!      → handler receives { success: true, transaction: "<signature>" }
//!      → handler independently verifies the payment on-chain
//!      → checkout is marked completed

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
// use serde_json::json;  // unused — remove if not needed
use base64::Engine;
use std::str::FromStr;

use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::config::RpcTransactionConfig;
use solana_signature::Signature;
use solana_transaction_status_client_types::{
    option_serializer::OptionSerializer, UiTransactionEncoding,
};

use crate::models::checkout::Checkout;
use super::{PaymentError, PaymentHandler};

/// x402 PaymentRequirements body (v2 spec).
/// This is what the server returns in the 402 response so the agent
/// knows where to send the payment and in what amount.
///
/// Shape matches `type PaymentRequirements` in
/// node_modules/@x402/core/dist/esm/x402Client-*.d.mts — v2 dropped
/// the v1 fields `resource`, `description`, and `mimeType` (those now
/// live only on the outer `PaymentRequired` envelope), and renamed
/// `maxAmountRequired` to `amount`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PaymentRequirements {
    scheme: &'static str,
    network: String,
    asset: String,
    amount: String,
    pay_to: String,
    max_timeout_seconds: u32,
    extra: Extra,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Extra {
    name: &'static str,
    version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    fee_payer: Option<String>,
    instructions_url: String,
}

/// Payload the agent sends in the X-Payment / PAYMENT-SIGNATURE header
/// (base64-encoded JSON).
///
/// Shape matches `type PaymentPayload` in
/// node_modules/@x402/core/dist/esm/x402Client-*.d.mts — in v2,
/// `scheme` and `network` are NOT top-level fields; they live nested
/// inside `accepted`, which is the full `PaymentRequirements` object
/// the client selected (an echo of what we sent in the 402). We keep
/// `accepted` as a raw `Value` since we only need to read a couple of
/// fields out of it and don't want a second struct that has to be kept
/// in lockstep with `PaymentRequirements`.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PaymentPayload {
    x402_version: u8,
    #[serde(default)]
    resource: Option<serde_json::Value>,
    #[serde(default)]
    extensions: Option<serde_json::Value>,
    accepted: serde_json::Value,
    payload: PayloadInner,
}

#[derive(Debug, Deserialize, Serialize)]
struct PayloadInner {
    /// Base64-encoded serialized Solana transaction.
    transaction: String,
}

/// Request body for POST /settle on the facilitator.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SettleRequest {
    payment_payload: serde_json::Value,
    payment_requirements: serde_json::Value,
}

/// Response from POST /settle on the facilitator.
#[derive(Debug, Deserialize)]
struct SettleResponse {
    success: bool,
    transaction: String,
    #[serde(default)]
    error_reason: Option<String>,
}

pub struct X402SolanaHandler {
    /// HTTP client reused across requests (connection pooling).
    client: reqwest::Client,
    /// URL of the x402 facilitator (Kora demo facilitator).
    /// e.g. "http://localhost:3001" (facilitator port from docker-compose)
    facilitator_url: String,
    /// Merchant wallet address that receives USDC payments.
    merchant_wallet: String,
    /// USDC mint address on devnet.
    usdc_mint: String,
    /// CAIP-2 network identifier.
    network: String,
    /// Base URL of this merchant server (for building self-referential
    /// links, e.g. instructionsUrl in the 402 response).
    base_url: String,
    /// Solana RPC endpoint used to independently verify settled
    /// payments on-chain — deliberately NOT the facilitator or Kora,
    /// so a compromised or malicious intermediary can't lie about
    /// whether the merchant actually got paid.
    solana_rpc_url: String,
}

impl X402SolanaHandler {
    pub fn new(
        facilitator_url: String,
        merchant_wallet: String,
        base_url: String,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            facilitator_url,
            merchant_wallet,
            // USDC devnet mint — confirmed in your token accounts
            usdc_mint: "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU".to_string(),
            // CAIP-2 identifier for Solana devnet, as required by
            // @x402/svm's `normalizeNetwork`
            // (node_modules/@x402/svm/dist/cjs/index.js:122). This is
            // NOT a human-readable alias like "solana:devnet" — it's
            // `solana:<genesis-hash-prefix>`, and the client rejects
            // anything not in its SOLANA_MAINNET_CAIP2/DEVNET/TESTNET
            // allowlist. Source: @x402/svm SOLANA_DEVNET_CAIP2 constant.
            network: "solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1".to_string(),
            base_url,
            solana_rpc_url: std::env::var("SOLANA_RPC_URL")
                .unwrap_or_else(|_| "https://api.devnet.solana.com".to_string()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct SupportedResponse {
    kinds: Vec<SupportedKind>,
}

#[derive(Debug, Deserialize)]
struct SupportedKind {
    #[serde(default)]
    extra: Option<serde_json::Value>,
}

impl X402SolanaHandler {
    /// Queries GET /supported on the facilitator to discover Kora's
    /// current fee payer. Does not fail the checkout if the facilitator
    /// is down — the buyer can still discover it independently.
    async fn fetch_fee_payer(&self) -> Option<String> {
        let url = format!("{}/supported", self.facilitator_url);
        let resp = self.client.get(&url).send().await.ok()?;
        let body: SupportedResponse = resp.json().await.ok()?;
        body.kinds
            .first()?
            .extra
            .as_ref()?
            .get("feePayer")?
            .as_str()
            .map(|s| s.to_string())
    }

    /// Independently verifies, against the blockchain, that a settled
    /// transaction actually transferred `required_amount` (or more) of
    /// the correct mint to `merchant_wallet`.
    ///
    /// Deliberately does NOT trust `settle_resp.success` from the
    /// facilitator — a compromised or malicious facilitator/Kora could
    /// lie about whether the payment landed.
    async fn verify_payment_on_chain(
        &self,
        tx_signature: &str,
        required_amount: u64,
    ) -> Result<(), PaymentError> {
        // 1. Parse the signature.
        let signature = Signature::from_str(tx_signature)
            .map_err(|e| PaymentError::Invalid(format!("invalid tx signature: {e}")))?;

        // 2. Query the transaction directly from the RPC — never from
        //    the facilitator. commitment: None defaults to Finalized
        //    (the correct level for real money), but Finalized status
        //    takes ~10-15s to be reached after Kora submits the
        //    transaction — querying immediately after settle almost
        //    always returns null. Poll with backoff instead of
        //    querying once.
        //
        //    Also note: @x402/svm builds versioned (v0) transactions.
        //    Without max_supported_transaction_version, most RPCs —
        //    including the public Solana devnet endpoint — silently
        //    return null instead of the transaction, which serde then
        //    fails to deserialize.
        let rpc = RpcClient::new(self.solana_rpc_url.clone());

        let config = RpcTransactionConfig {
            encoding: Some(UiTransactionEncoding::JsonParsed),
            max_supported_transaction_version: Some(0),
            commitment: None,
        };

        const MAX_ATTEMPTS: u32 = 10;
        const INITIAL_DELAY_MS: u64 = 1_000;

        let mut confirmed_tx = None;
        let mut last_err = None;

        for attempt in 0..MAX_ATTEMPTS {
            match rpc.get_transaction_with_config(&signature, config.clone()).await {
                Ok(tx) => {
                    confirmed_tx = Some(tx);
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    if attempt + 1 < MAX_ATTEMPTS {
                        // Exponential backoff: 1s, 2s, 4s, 8s... capped at 8s.
                        let delay_ms = (INITIAL_DELAY_MS * 2u64.pow(attempt)).min(8_000);
                        tracing::info!(
                            tx_signature = %tx_signature,
                            attempt = attempt + 1,
                            max_attempts = MAX_ATTEMPTS,
                            delay_ms,
                            "transaction not yet finalized, retrying"
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    }
                }
            }
        }

        let confirmed_tx = confirmed_tx.ok_or_else(|| {
            PaymentError::Facilitator(format!(
                "failed to fetch tx from Solana RPC after {MAX_ATTEMPTS} attempts: {}",
                last_err.map(|e| e.to_string()).unwrap_or_default()
            ))
        })?;

        let meta = confirmed_tx.transaction.meta.ok_or_else(|| {
            PaymentError::Invalid("transaction has no metadata (not yet confirmed?)".to_string())
        })?;

        // 3. The transaction must have succeeded on-chain, not just
        //    landed in a block.
        if meta.err.is_some() {
            return Err(PaymentError::Invalid(format!(
                "transaction failed on-chain: {:?}",
                meta.err
            )));
        }

        // 4. Extract pre/post token balances. OptionSerializer is the
        //    wrapper the SDK uses to distinguish "field absent" from
        //    "field null" in the RPC's JSON response.
        let pre_balances: Vec<_> = match meta.pre_token_balances {
            OptionSerializer::Some(v) => v,
            _ => vec![],
        };
        let post_balances: Vec<_> = match meta.post_token_balances {
            OptionSerializer::Some(v) => v,
            _ => vec![],
        };

        // 5. Find the merchant's entry: same mint, same owner. We
        //    filter by `owner`, not by token account address, because
        //    the ATA is derived — what matters is who controls it, not
        //    its exact address.
        let find_merchant_balance = |balances: &[solana_transaction_status_client_types::UiTransactionTokenBalance]| -> u64 {
            balances
                .iter()
                .find(|b| {
                    b.mint == self.usdc_mint
                        && matches!(&b.owner, OptionSerializer::Some(o) if o == &self.merchant_wallet)
                })
                .and_then(|b| b.ui_token_amount.amount.parse::<u64>().ok())
                .unwrap_or(0)
        };

        let pre_amount = find_merchant_balance(&pre_balances);
        let post_amount = find_merchant_balance(&post_balances);

        // 6. What matters is the delta, not the absolute balance.
        let delta = post_amount.saturating_sub(pre_amount);

        if delta < required_amount {
            return Err(PaymentError::Mismatch(format!(
                "on-chain transfer to merchant was {delta}, expected at least {required_amount}"
            )));
        }

        tracing::info!(
            tx_signature = %tx_signature,
            delta = delta,
            required = required_amount,
            "on-chain payment verification passed"
        );

        Ok(())
    }
}

#[async_trait]
impl PaymentHandler for X402SolanaHandler {
    fn handler_id(&self) -> &str {
        "x402_solana_devnet"
    }

    async fn payment_requirements(
        &self,
        checkout: &Checkout,
    ) -> Result<serde_json::Value, PaymentError> {
        // total is stored in cents/smallest unit — for USDC (6 decimals)
        // we need to convert: 1 USD = 1_000_000 USDC micro-units
        // For simplicity in this PoC we treat total as USDC micro-units directly
        let amount = checkout.total.to_string();
        let fee_payer = self.fetch_fee_payer().await;

        let requirements = PaymentRequirements {
            scheme: "exact",
            network: self.network.clone(),
            asset: self.usdc_mint.clone(),
            amount,
            pay_to: self.merchant_wallet.clone(),
            max_timeout_seconds: 300,
            extra: Extra {
                name: "USDC",
                version: "1",
                fee_payer,
                instructions_url: format!("{}/docs/skills/ucp-buyer.md", self.base_url),
            },
        };

        serde_json::to_value(requirements)
            .map_err(|e| PaymentError::Internal(e.to_string()))
    }

    async fn verify_and_settle(
        &self,
        checkout: &Checkout,
        payment_header: &str,
    ) -> Result<(), PaymentError> {
        // 1. Decode the X-Payment / PAYMENT-SIGNATURE header (base64 JSON)
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(payment_header)
            .map_err(|e| PaymentError::Invalid(format!("base64 decode failed: {e}")))?;

        let payload: PaymentPayload = serde_json::from_slice(&decoded)
            .map_err(|e| PaymentError::Invalid(format!("invalid payment payload: {e}")))?;

        // 2. Basic sanity checks before calling the facilitator.
        //    In v2, scheme/network aren't top-level fields on the
        //    payload — they're nested inside `accepted`
        //    (see PaymentPayload doc comment above).
        let scheme = payload
            .accepted
            .get("scheme")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let network = payload
            .accepted
            .get("network")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        if scheme != "exact" {
            return Err(PaymentError::Invalid(format!(
                "unsupported scheme: {scheme}"
            )));
        }
        if network != self.network {
            return Err(PaymentError::Mismatch(format!(
                "expected network {}, got {network}",
                self.network
            )));
        }

        // 3. Build the settle request — re-serialize checkout requirements
        //    so the facilitator can cross-check amount/recipient
        let requirements = self.payment_requirements(checkout).await?;

        let settle_req = SettleRequest {
            payment_payload: serde_json::to_value(&payload)
                .map_err(|e| PaymentError::Internal(e.to_string()))?,
            payment_requirements: requirements,
        };

        // 4. Call the facilitator POST /settle
        let url = format!("{}/settle", self.facilitator_url);
        let response = self
            .client
            .post(&url)
            .json(&settle_req)
            .send()
            .await
            .map_err(|e| PaymentError::Facilitator(format!("facilitator unreachable: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(PaymentError::Facilitator(format!(
                "facilitator returned {status}: {body}"
            )));
        }

        // 5. Parse the settle response
        let settle_resp: SettleResponse = response
            .json()
            .await
            .map_err(|e| PaymentError::Facilitator(format!("invalid facilitator response: {e}")))?;

        if !settle_resp.success {
            return Err(PaymentError::Invalid(
                settle_resp
                    .error_reason
                    .unwrap_or_else(|| "settlement failed".to_string()),
            ));
        }

        // 6. Independently verify the payment on-chain — never trust
        //    settle_resp.success alone.
        tracing::info!(
            checkout_id = %checkout.id,
            tx_signature = %settle_resp.transaction,
            "attempting on-chain verification"
        );

        self.verify_payment_on_chain(&settle_resp.transaction, checkout.total)
            .await?;

        tracing::info!(
            checkout_id = %checkout.id,
            tx_signature = %settle_resp.transaction,
            "x402 payment settled on Solana devnet"
        );

        Ok(())
    }
}
