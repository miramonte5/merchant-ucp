use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde_json::json;

use crate::{
    models::checkout::{Checkout, CreateCheckoutRequest, UpdateCheckoutRequest},
    AppState,
};

/// POST /ucp/v1/checkout-sessions
pub async fn create_checkout(
    State(state): State<AppState>,
    Json(req): Json<CreateCheckoutRequest>,
) -> impl IntoResponse {
    let checkout = Checkout::new(req);
    state.checkout_store.insert(checkout.clone());
    (StatusCode::CREATED, Json(checkout))
}

/// GET /ucp/v1/checkout-sessions/:id
pub async fn get_checkout(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.checkout_store.get(&id) {
        Some(checkout) => (StatusCode::OK, Json(checkout)).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "checkout not found" })),
        )
            .into_response(),
    }
}

/// PUT /ucp/v1/checkout-sessions/:id
pub async fn update_checkout(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(update): Json<UpdateCheckoutRequest>,
) -> impl IntoResponse {
    match state
        .checkout_store
        .update_with(&id, |c| c.apply_update(update))
    {
        Some(checkout) => (StatusCode::OK, Json(checkout)).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "checkout not found" })),
        )
            .into_response(),
    }
}

/// POST /ucp/v1/checkout-sessions/:id/complete
pub async fn complete_checkout(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Some(checkout) = state.checkout_store.get(&id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "checkout not found" })),
        )
            .into_response();
    };
    let _ = checkout; // existence already confirmed above

    let mut complete_error = None;
    let updated = state.checkout_store.update_with(&id, |c| {
        if let Err(msg) = c.complete() {
            complete_error = Some(msg);
        }
    });

    match (updated, complete_error) {
        (Some(checkout), None) => (StatusCode::OK, Json(checkout)).into_response(),
        (Some(checkout), Some(_msg)) => {
            // complete() failed but didn't mutate status; surface the checkout
            // with its existing messages so the agent can see why.
            (StatusCode::CONFLICT, Json(checkout)).into_response()
        }
        (None, _) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "checkout not found" })),
        )
            .into_response(),
    }
}

/// POST /ucp/v1/checkout-sessions/:id/cancel
pub async fn cancel_checkout(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.checkout_store.update_with(&id, |c| c.cancel()) {
        Some(checkout) => (StatusCode::OK, Json(checkout)).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "checkout not found" })),
        )
            .into_response(),
    }
}
