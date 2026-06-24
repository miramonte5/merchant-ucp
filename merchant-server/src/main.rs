mod models;
mod routes;
mod store;

use axum::{
    routing::{get, post},
    Router,
};
use std::net::SocketAddr;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use store::CheckoutStore;

/// Shared application state, cloned into every request handler.
/// `base_url` is needed to build absolute URLs in the UCP profile response.
/// `checkout_store` holds in-memory checkout sessions (Phase 1).
#[derive(Clone)]
pub struct AppState {
    pub base_url: String,
    pub checkout_store: CheckoutStore,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let base_url = std::env::var("BASE_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());
    let state = AppState {
        base_url: base_url.clone(),
        checkout_store: CheckoutStore::new(),
    };

    let app = Router::new()
        .route("/.well-known/ucp", get(routes::well_known::well_known_ucp))
        .route(
            "/ucp/v1/checkout-sessions",
            post(routes::checkout::create_checkout),
        )
        .route(
            "/ucp/v1/checkout-sessions/{id}",
            get(routes::checkout::get_checkout).put(routes::checkout::update_checkout),
        )
        .route(
            "/ucp/v1/checkout-sessions/{id}/complete",
            post(routes::checkout::complete_checkout),
        )
        .route(
            "/ucp/v1/checkout-sessions/{id}/cancel",
            post(routes::checkout::cancel_checkout),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    tracing::info!("merchant-server listening on {addr}, base_url={base_url}");

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
