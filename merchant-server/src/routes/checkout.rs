use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
};
use base64::Engine;
use serde_json::json;

use crate::{
    models::checkout::{Checkout, CreateCheckoutRequest, UpdateCheckoutRequest},
    store::StoreError,
    AppState,
};

fn not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "checkout not found" })),
    )
}

fn internal_error() -> impl IntoResponse {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "internal server error" })),
    )
}

/// POST /ucp/v1/checkout-sessions
pub async fn create_checkout(
    State(state): State<AppState>,
    Json(req): Json<CreateCheckoutRequest>,
) -> impl IntoResponse {
    let checkout = Checkout::new(req);

    match state.checkout_store.insert(checkout.clone()).await {
        Ok(_) => (StatusCode::CREATED, Json(checkout)).into_response(),
        Err(_) => internal_error().into_response(),
    }
}

/// GET /ucp/v1/checkout-sessions/:id
pub async fn get_checkout(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.checkout_store.get(&id).await {
        Ok(checkout) => (StatusCode::OK, Json(checkout)).into_response(),
        Err(StoreError::NotFound) => not_found().into_response(),
        Err(_) => internal_error().into_response(),
    }
}

/// PUT /ucp/v1/checkout-sessions/:id
pub async fn update_checkout(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(update): Json<UpdateCheckoutRequest>,
) -> impl IntoResponse {
    let mut checkout = match state.checkout_store.get(&id).await {
        Ok(c) => c,
        Err(StoreError::NotFound) => return not_found().into_response(),
        Err(_) => return internal_error().into_response(),
    };

    checkout.apply_update(update);

    match state.checkout_store.save(&checkout).await {
        Ok(_) => (StatusCode::OK, Json(checkout)).into_response(),
        Err(_) => internal_error().into_response(),
    }
}

/// POST /ucp/v1/checkout-sessions/:id/complete
///
/// Two-phase flow depending on the checkout's payment_handler_id:
///
/// Phase A — no X-Payment header:
///   → handler.payment_requirements() → 402 with payment instructions,
///     carried in the `PAYMENT-REQUIRED` header (base64-encoded JSON),
///     per x402 v2. The JSON body is kept only as a human-readable
///     hint, not as the source of truth for the client library.
///
/// Phase B — X-Payment header present:
///   → handler.verify_and_settle() → 200 if settled, 402/400 if not
///
/// Mock handler bypasses both phases and completes immediately.
pub async fn complete_checkout(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let mut checkout = match state.checkout_store.get(&id).await {
        Ok(c) => c,
        Err(StoreError::NotFound) => return not_found().into_response(),
        Err(_) => return internal_error().into_response(),
    };

    // Resolve the payment handler for this checkout.
    let handler_id = match &checkout.payment_handler_id {
        Some(id) => id.clone(),
        None => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "error": "no payment handler selected",
                    "hint": "update the checkout with a payment_handler_id first"
                })),
            )
                .into_response()
        }
    };

    // Mock handler — bypasses payment flow entirely (for testing only).
    // Not advertised in /.well-known/ucp for real agents.
    if handler_id == "mock_1" {
        let complete_result = checkout.complete();
        if state.checkout_store.save(&checkout).await.is_err() {
            return internal_error().into_response();
        }
        return match complete_result {
            Ok(_) => (StatusCode::OK, Json(checkout)).into_response(),
            Err(_) => (StatusCode::CONFLICT, Json(checkout)).into_response(),
        };
    }

    // Look up the real payment handler.
    let handler = match state.payment_handlers.get(&handler_id) {
        Some(h) => h.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": format!("unsupported payment handler: {handler_id}")
                })),
            )
                .into_response()
        }
    };
    // Extract the payment header if present. x402 v2 sends it as
    // `PAYMENT-SIGNATURE`; v1 sent it as `X-Payment`
    // (node_modules/@x402/fetch/dist/cjs/index.js, encodePaymentSignatureHeader
    // switch on x402Version). We check both so v1 clients still work.
    let x_payment = headers
        .get("PAYMENT-SIGNATURE")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    match x_payment {
        // Phase A: no payment yet — return requirements so agent can pay.
        None => {
            match handler.payment_requirements(&checkout).await {
                Ok(requirements) => {
                    // x402 v2 puts the PaymentRequired object in the
                    // `PAYMENT-REQUIRED` header (base64-encoded JSON),
                    // not in the response body. The client library
                    // (@x402/core) only reads the body as a v1
                    // compatibility fallback (x402Version === 1).
                    let resource_url = requirements
                        .get("resource")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();

                    let payment_required = json!({
                        "x402Version": 2,
                        "error": "payment required",
                        "resource": { "url": resource_url },
                        "accepts": [requirements],
                    });

                    let encoded = base64::engine::general_purpose::STANDARD
                        .encode(payment_required.to_string());

                    let mut resp_headers = HeaderMap::new();
                    match encoded.parse() {
                        Ok(value) => {
                            resp_headers.insert("PAYMENT-REQUIRED", value);
                        }
                        Err(_) => return internal_error().into_response(),
                    }

                    (
                        StatusCode::PAYMENT_REQUIRED,
                        resp_headers,
                        Json(json!({ "error": "payment required" })),
                    )
                        .into_response()
                }
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": e.to_string() })),
                )
                    .into_response(),
            }
        }

        // Phase B: agent sent payment — verify and settle.
        Some(payment_header) => {
            match handler.verify_and_settle(&checkout, &payment_header).await {
                Ok(()) => {
                    // Payment settled — mark checkout as completed.
                    let complete_result = checkout.complete();
                    if state.checkout_store.save(&checkout).await.is_err() {
                        return internal_error().into_response();
                    }
                    match complete_result {
                        Ok(_) => (StatusCode::OK, Json(checkout)).into_response(),
                        Err(_) => (StatusCode::CONFLICT, Json(checkout)).into_response(),
                    }
                }
                Err(e) => {
                    // Payment failed — return 402 with the reason.
                    (
                        StatusCode::PAYMENT_REQUIRED,
                        Json(json!({
                            "x402Version": 2,
                            "error": e.to_string()
                        })),
                    )
                        .into_response()
                }
            }
        }
    }
}

/// POST /ucp/v1/checkout-sessions/:id/cancel
pub async fn cancel_checkout(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let mut checkout = match state.checkout_store.get(&id).await {
        Ok(c) => c,
        Err(StoreError::NotFound) => return not_found().into_response(),
        Err(_) => return internal_error().into_response(),
    };

    checkout.cancel();

    match state.checkout_store.save(&checkout).await {
        Ok(_) => (StatusCode::OK, Json(checkout)).into_response(),
        Err(_) => internal_error().into_response(),
    }
}
