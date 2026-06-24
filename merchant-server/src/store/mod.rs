use crate::models::checkout::Checkout;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// In-memory checkout storage for Phase 1.
/// Replaced by PostgreSQL in Phase 2 — see docs/architecture.md.
///
/// Wrapped in Arc<RwLock<..>> so it can be cloned cheaply into AppState
/// and shared safely across concurrent request handlers.
#[derive(Clone, Default)]
pub struct CheckoutStore {
    inner: Arc<RwLock<HashMap<String, Checkout>>>,
}

impl CheckoutStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, checkout: Checkout) {
        self.inner
            .write()
            .expect("checkout store lock poisoned")
            .insert(checkout.id.clone(), checkout);
    }

    pub fn get(&self, id: &str) -> Option<Checkout> {
        self.inner
            .read()
            .expect("checkout store lock poisoned")
            .get(id)
            .cloned()
    }

    /// Applies `f` to the checkout if it exists, persisting the mutation,
    /// and returns the updated checkout.
    pub fn update_with<F>(&self, id: &str, f: F) -> Option<Checkout>
    where
        F: FnOnce(&mut Checkout),
    {
        let mut guard = self.inner.write().expect("checkout store lock poisoned");
        let checkout = guard.get_mut(id)?;
        f(checkout);
        Some(checkout.clone())
    }
}
