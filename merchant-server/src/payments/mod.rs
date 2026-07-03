//! Payment handler abstraction layer.
//!
//! Each payment method (x402/Solana, MPP/Stripe, etc.) implements the
//! `PaymentHandler` trait. The merchant server routes payment processing
//! to the appropriate handler based on the checkout's `payment_handler_id`,
//! without knowing the details of any specific payment method.
//!
//! Phase 3 handlers:
//!   - `x402_solana`  → x402SolanaHandler  (src/payments/x402_solana.rs)
//!   - `mpp_stripe`   → MppHandler         (src/payments/mpp.rs)

mod x402_solana;
mod mpp;  

pub use x402_solana::X402SolanaHandler;
pub use mpp::MppHandler; 

use async_trait::async_trait;
use crate::models::checkout::Checkout;

/// Errors that a payment handler can return.
#[derive(Debug, thiserror::Error)]
pub enum PaymentError {
    /// Payment payload is malformed or failed facilitator verification.
    #[error("invalid payment: {0}")]
    Invalid(String),

    /// Payment amount or currency does not match the checkout total.
    #[error("payment mismatch: {0}")]
    Mismatch(String),

    /// Facilitator or external service is unreachable.
    #[error("facilitator error: {0}")]
    Facilitator(String),

    /// Catch-all for unexpected errors.
    #[error("internal payment error: {0}")]
    Internal(String),
}

/// The interface every payment handler must implement.
///
/// Handlers are registered in `AppState` keyed by their `handler_id`.
/// The `complete_checkout` route resolves the handler from the checkout's
/// `payment_handler_id` and delegates all payment logic to it.
#[async_trait]
pub trait PaymentHandler: Send + Sync {
    /// The id this handler is registered under in the UCP profile and
    /// in `AppState::payment_handlers`.
    /// e.g. "x402_solana_devnet", "mpp_stripe"
    fn handler_id(&self) -> &str;

    /// Called when `complete` is requested but no `X-Payment` header is
    /// present. Returns the 402 body the agent must respond to — the
    /// exact shape depends on the payment scheme but always includes
    /// enough information for the agent (or a human) to satisfy the
    /// payment requirement.
    async fn payment_requirements(
        &self,
        checkout: &Checkout,
    ) -> Result<serde_json::Value, PaymentError>;

    /// Called when `complete` is requested WITH an `X-Payment` header.
    /// Verifies the payment via the appropriate facilitator and settles
    /// it on-chain or with the payment processor.
    ///
    /// Returns `Ok(())` if the payment is valid and settled — the caller
    /// can then mark the checkout as `completed`.
    /// Returns `Err(PaymentError)` if verification or settlement failed.
    async fn verify_and_settle(
        &self,
        checkout: &Checkout,
        payment_header: &str,
    ) -> Result<(), PaymentError>;
}
