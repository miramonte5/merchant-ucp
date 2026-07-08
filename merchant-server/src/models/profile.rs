use serde::Serialize;
use std::collections::HashMap;

/// Top-level response for GET /.well-known/ucp
#[derive(Serialize)]
pub struct UcpDiscoveryDocument {
    pub ucp: UcpProfile,
}

#[derive(Serialize)]
pub struct UcpProfile {
    pub version: String,
    pub services: HashMap<String, Vec<ServiceDescriptor>>,
    pub capabilities: HashMap<String, Vec<CapabilityDescriptor>>,
    pub payment_handlers: HashMap<String, Vec<PaymentHandlerDescriptor>>,
}

#[derive(Serialize)]
pub struct ServiceDescriptor {
    pub version: String,
    pub transport: String,
    pub endpoint: String,
}

#[derive(Serialize)]
pub struct CapabilityDescriptor {
    pub version: String,
}

#[derive(Serialize)]
pub struct PaymentHandlerDescriptor {
    pub id: String,
    pub version: String,
}

impl UcpDiscoveryDocument {
    /// Builds the merchant's UCP profile for the PoC.
    /// `base_url` is the externally reachable origin of this server,
    /// e.g. "http://localhost:3000" in local dev.
    pub fn for_merchant(base_url: &str) -> Self {
        let mut services = HashMap::new();
        services.insert(
            "dev.ucp.shopping".to_string(),
            vec![ServiceDescriptor {
                version: "2026-04-08".to_string(),
                transport: "rest".to_string(),
                endpoint: format!("{base_url}/ucp/v1"),
            }],
        );

        let mut capabilities = HashMap::new();
        capabilities.insert(
            "dev.ucp.shopping.checkout".to_string(),
            vec![CapabilityDescriptor {
                version: "2026-04-08".to_string(),
            }],
        );

        // Placeholder handler for Phase 1. Real handlers (Stripe, x402/USDC)
        // are added in Phase 3 — see docs/architecture.md.
        let mut payment_handlers = HashMap::new();
        payment_handlers.insert(
            "dev.cuadrolabs.mock".to_string(),
            vec![PaymentHandlerDescriptor {
                id: "mock_1".to_string(),
                version: "2026-04-08".to_string(),
            }],
        );
        payment_handlers.insert(
            "dev.cuadrolabs.x402".to_string(),
            vec![PaymentHandlerDescriptor {
                id: "x402_solana_devnet".to_string(),
                version: "2026-04-08".to_string(),
            }],
        );

        UcpDiscoveryDocument {
            ucp: UcpProfile {
                version: "2026-04-08".to_string(),
                services,
                capabilities,
                payment_handlers,
            },
        }
    }
}
