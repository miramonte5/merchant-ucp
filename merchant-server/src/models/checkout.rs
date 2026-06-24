use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Lifecycle states of a checkout session, per UCP spec 2026-04-08.
///
/// incomplete -> ready_for_complete -> complete_in_progress -> completed
///     ^
///     | (can bounce here when buyer input/review is needed)
///     v
/// requires_escalation
///
/// canceled can occur from any state (e.g. session expiry).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutStatus {
    Incomplete,
    ReadyForComplete,
    CompleteInProgress,
    Completed,
    RequiresEscalation,
    Canceled,
}

/// Severity of a checkout message, determines what the buyer/agent must do.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageSeverity {
    /// Agent can fix this itself via Update Checkout and retry.
    Recoverable,
    /// Needs human input — hand off via continue_url.
    RequiresBuyerInput,
    /// Needs human review/approval — hand off via continue_url.
    RequiresBuyerReview,
    /// Cannot be fixed in this session — start a new checkout.
    Unrecoverable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckoutMessage {
    pub severity: MessageSeverity,
    pub content: String,
}

/// A single item being purchased.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineItem {
    pub id: String,
    pub title: String,
    pub quantity: u32,
    /// Price per unit, in the smallest currency unit (e.g. cents for USD).
    pub unit_price: u64,
    pub currency: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Buyer {
    pub name: Option<String>,
    pub email: Option<String>,
}

/// Request body for POST /ucp/v1/checkout-sessions
#[derive(Debug, Clone, Deserialize)]
pub struct CreateCheckoutRequest {
    pub line_items: Vec<LineItem>,
    #[serde(default)]
    pub buyer: Buyer,
}

/// Request body for PUT /ucp/v1/checkout-sessions/:id
/// All fields optional — the agent only sends what it wants to update.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct UpdateCheckoutRequest {
    pub line_items: Option<Vec<LineItem>>,
    pub buyer: Option<Buyer>,
    pub payment_handler_id: Option<String>,
}

/// The full checkout session resource, returned by Create/Get/Update/Complete.
#[derive(Debug, Clone, Serialize)]
pub struct Checkout {
    pub id: String,
    pub status: CheckoutStatus,
    pub line_items: Vec<LineItem>,
    pub buyer: Buyer,
    pub total: u64,
    pub currency: String,
    pub messages: Vec<CheckoutMessage>,
    /// Present only when status == RequiresEscalation.
    pub continue_url: Option<String>,
    /// Payment handler selected for this checkout, if any.
    pub payment_handler_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Checkout {
    pub fn new(req: CreateCheckoutRequest) -> Self {
        let now = Utc::now();
        let total = Self::calculate_total(&req.line_items);
        let currency = req
            .line_items
            .first()
            .map(|li| li.currency.clone())
            .unwrap_or_else(|| "USD".to_string());

        Checkout {
            id: format!("chk_{}", Uuid::new_v4().simple()),
            status: CheckoutStatus::Incomplete,
            line_items: req.line_items,
            buyer: req.buyer,
            total,
            currency,
            messages: Vec::new(),
            continue_url: None,
            payment_handler_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn calculate_total(line_items: &[LineItem]) -> u64 {
        line_items
            .iter()
            .map(|li| li.unit_price * li.quantity as u64)
            .sum()
    }

    /// Applies an update and re-evaluates whether the checkout is ready
    /// to complete. This is intentionally simple for Phase 1 — real
    /// validation (e.g. payment handler availability) comes later.
    pub fn apply_update(&mut self, update: UpdateCheckoutRequest) {
        if let Some(line_items) = update.line_items {
            self.total = Self::calculate_total(&line_items);
            self.line_items = line_items;
        }
        if let Some(buyer) = update.buyer {
            self.buyer = buyer;
        }
        if let Some(handler_id) = update.payment_handler_id {
            self.payment_handler_id = Some(handler_id);
        }
        self.updated_at = Utc::now();
        self.refresh_status();
    }

    /// Phase 1 readiness rule: ready once there's at least one line item,
    /// a buyer email, and a payment handler selected.
    fn refresh_status(&mut self) {
        if self.status == CheckoutStatus::Canceled || self.status == CheckoutStatus::Completed {
            return;
        }

        let has_items = !self.line_items.is_empty();
        let has_buyer_email = self.buyer.email.is_some();
        let has_payment_handler = self.payment_handler_id.is_some();

        self.status = if has_items && has_buyer_email && has_payment_handler {
            CheckoutStatus::ReadyForComplete
        } else {
            CheckoutStatus::Incomplete
        };
    }

    pub fn complete(&mut self) -> Result<(), CheckoutMessage> {
        if self.status != CheckoutStatus::ReadyForComplete {
            return Err(CheckoutMessage {
                severity: MessageSeverity::Recoverable,
                content: format!(
                    "Checkout cannot be completed from status {:?}",
                    self.status
                ),
            });
        }
        self.status = CheckoutStatus::Completed;
        self.updated_at = Utc::now();
        Ok(())
    }

    pub fn cancel(&mut self) {
        self.status = CheckoutStatus::Canceled;
        self.updated_at = Utc::now();
    }
}
