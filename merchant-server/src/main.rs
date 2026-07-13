mod models;
mod payments;
mod routes;
mod store;

use axum::{
    routing::{get, post},
    Router,
};
use sqlx::postgres::PgPoolOptions;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::trace::TraceLayer;
use tower_http::services::ServeDir;
use tracing_subscriber::EnvFilter;

use payments::{MppHandler, PaymentHandler, X402SolanaHandler};
use store::{CheckoutStore, PgCheckoutStore};

/// Shared application state, cloned into every request handler.
#[derive(Clone)]
pub struct AppState {
    pub base_url: String,
    pub checkout_store: Arc<dyn CheckoutStore>,
    /// Payment handlers keyed by their handler_id.
    /// Routes resolve the correct handler from checkout.payment_handler_id.
    pub payment_handlers: HashMap<String, Arc<dyn PaymentHandler>>,
}

#[tokio::main]
async fn main() {

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let base_url = std::env::var("BASE_URL")
        .unwrap_or_else(|_| "http://localhost:3000".to_string());

    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set");

    let facilitator_url = std::env::var("FACILITATOR_URL")
        .unwrap_or_else(|_| "http://localhost:3001".to_string());

    let merchant_wallet = std::env::var("MERCHANT_WALLET")
        .expect("MERCHANT_WALLET must be set (Solana public key receiving USDC payments)");

    // PostgreSQL connection pool.
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres");

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("failed to run database migrations");

    // Register payment handlers.
    let mut payment_handlers: HashMap<String, Arc<dyn PaymentHandler>> = HashMap::new();

    let x402 = X402SolanaHandler::new(facilitator_url, merchant_wallet, base_url.clone());
    payment_handlers.insert(x402.handler_id().to_string(), Arc::new(x402));

    let mpp = MppHandler::new(base_url.clone());
    payment_handlers.insert(mpp.handler_id().to_string(), Arc::new(mpp));

    let state = AppState {
        base_url: base_url.clone(),
        checkout_store: Arc::new(PgCheckoutStore::new(pool)),
        payment_handlers,
    };

    let app = Router::new()
        .route("/.well-known/ucp", get(routes::well_known::well_known_ucp))
        .nest_service(
            "/docs/skills", 
            ServeDir::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../docs/skills")),
        )
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
