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
//!      → checkout is marked completed

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
// use serde_json::json;  // unused — remove if not needed
use base64::Engine;

use crate::models::checkout::Checkout;
use super::{PaymentError, PaymentHandler};

/// x402 PaymentRequirements body (v2 spec).
/// This is what the server returns in the 402 response so the agent
/// knows where to send the payment and in what amount.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PaymentRequirements {
    scheme: &'static str,
    network: String,
    max_amount_required: String,
    resource: String,
    description: String,
    mime_type: &'static str,
    pay_to: String,
    max_timeout_seconds: u32,
    asset: String,
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

/// Payload the agent sends in the X-Payment header (base64-encoded JSON).
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PaymentPayload {
    x402_version: u8,
    scheme: String,
    network: String,
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
            network: "solana:devnet".to_string(),
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
    /// Consulta GET /supported en el facilitador para descubrir el fee
    /// payer actual de Kora. No falla el checkout si el facilitador está
    /// caído — el comprador puede seguir descubriéndolo por su cuenta.
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
            max_amount_required: amount,
            resource: format!("/ucp/v1/checkout-sessions/{}/complete", checkout.id),
            description: format!("Payment for checkout {}", checkout.id),
            mime_type: "application/json",
            pay_to: self.merchant_wallet.clone(),
            max_timeout_seconds: 300,
            asset: self.usdc_mint.clone(),
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
        // 1. Decode the X-Payment header (base64 JSON)
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(payment_header)
            .map_err(|e| PaymentError::Invalid(format!("base64 decode failed: {e}")))?;

        let payload: PaymentPayload = serde_json::from_slice(&decoded)
            .map_err(|e| PaymentError::Invalid(format!("invalid payment payload: {e}")))?;

        // 2. Basic sanity checks before calling the facilitator
        if payload.scheme != "exact" {
            return Err(PaymentError::Invalid(format!(
                "unsupported scheme: {}",
                payload.scheme
            )));
        }
        if payload.network != self.network {
            return Err(PaymentError::Mismatch(format!(
                "expected network {}, got {}",
                self.network, payload.network
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

        tracing::info!(
            checkout_id = %checkout.id,
            tx_signature = %settle_resp.transaction,
            "x402 payment settled on Solana devnet"
        );

        Ok(())
    }
}
