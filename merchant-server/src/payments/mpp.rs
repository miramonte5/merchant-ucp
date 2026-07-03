//! MPP (Monetization Payment Protocol) payment handler.
//!
//! MPP is an IETF draft protocol developed by Stripe and Tempo that supports
//! fiat and stablecoin payments in the same endpoint via session-based
//! pre-authorization. It is the recommended handler for fiat payments in
//! UCP-compliant merchant servers.
//!
//! Current status: stub implementation. The PaymentRequirements shape and
//! session model are defined; the actual MPP session API calls (Stripe/Tempo)
//! are marked as TODO and will be implemented in a dedicated sub-phase.
//!
//! References:
//!   - https://datatracker.ietf.org/doc/draft-ietf-httpbis-mpp/
//!   - https://stripe.com/docs/mpp

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::models::checkout::Checkout;
use super::{PaymentError, PaymentHandler};

/// MPP session kinds supported by this handler.
/// A "session" in MPP is a pre-authorized spending limit the agent obtains
/// from the payment provider before the merchant charges against it.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MppSessionKind {
    /// One-time payment via Stripe card or bank transfer.
    StripePaymentIntent,
    /// Pre-authorized spending session (BNPL, crypto, etc.) via Tempo.
    TempoSession,
}

/// The 402 body returned to the agent when MPP payment is required.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MppPaymentRequirements {
    /// Protocol identifier — agents use this to select the right handler.
    protocol: &'static str,
    /// Supported session kinds for this merchant.
    supported_kinds: Vec<MppSessionKind>,
    /// Amount required, in the currency's smallest unit (e.g. cents for USD).
    amount: u64,
    /// ISO 4217 currency code.
    currency: String,
    /// Merchant-generated session identifier for correlation.
    merchant_session_id: String,
    /// Where the agent should POST its payment session token.
    payment_endpoint: String,
}

pub struct MppHandler {
    /// Base URL of the merchant server, used to build `payment_endpoint`.
    base_url: String,
}

impl MppHandler {
    pub fn new(base_url: String) -> Self {
        Self { base_url }
    }
}

#[async_trait]
impl PaymentHandler for MppHandler {
    fn handler_id(&self) -> &str {
        "mpp_stripe"
    }

    async fn payment_requirements(
        &self,
        checkout: &Checkout,
    ) -> Result<serde_json::Value, PaymentError> {
        let requirements = MppPaymentRequirements {
            protocol: "mpp-draft-01",
            supported_kinds: vec![
                MppSessionKind::StripePaymentIntent,
                MppSessionKind::TempoSession,
            ],
            amount: checkout.total,
            currency: checkout.currency.clone(),
            merchant_session_id: checkout.id.clone(),
            payment_endpoint: format!(
                "{}/ucp/v1/checkout-sessions/{}/complete",
                self.base_url, checkout.id
            ),
        };

        serde_json::to_value(requirements)
            .map_err(|e| PaymentError::Internal(e.to_string()))
    }

    async fn verify_and_settle(
        &self,
        _checkout: &Checkout,
        _payment_header: &str,
    ) -> Result<(), PaymentError> {
        // TODO: implement MPP session verification
        //
        // The full flow when implemented:
        // 1. Decode the X-Payment header (MPP session token from Stripe/Tempo)
        // 2. POST to Stripe API: retrieve PaymentIntent by session token
        //    → confirm it matches checkout.total and checkout.currency
        //    → confirm status == "succeeded"
        // 3. OR POST to Tempo API: verify pre-authorized session
        //    → confirm spending limit covers checkout.total
        //    → debit the session for checkout.total
        // 4. Return Ok(()) on success, Err(PaymentError::Invalid(...)) on failure
        //
        // Stripe test mode endpoint: https://api.stripe.com/v1/payment_intents
        // Tempo sandbox: https://sandbox.tempo.eu/v1/sessions
        //
        // Both require API keys in .env:
        //   STRIPE_SECRET_KEY=sk_test_...
        //   TEMPO_API_KEY=...

        Err(PaymentError::Facilitator(
            "MPP integration not yet implemented — coming in Phase 3b".to_string(),
        ))
    }
}
