//! Service implementation for the e2e policy `AuthZ` resolver plugin.

use std::sync::Arc;

use toolkit_macros::domain_model;

use super::policy::Policy;

/// The plugin service: answers every evaluation from one compiled [`Policy`].
#[domain_model]
#[derive(Debug, Clone)]
pub struct Service {
    pub(super) policy: Arc<Policy>,
}

impl Service {
    /// Create a service over a compiled policy.
    #[must_use]
    pub fn new(policy: Policy) -> Self {
        Self {
            policy: Arc::new(policy),
        }
    }

    /// The compiled policy this service answers from.
    #[must_use]
    pub fn policy(&self) -> &Policy {
        &self.policy
    }
}
