//! Default-tolerant deployment configuration and versioned seller hold policy.
use std::num::{NonZeroU32, NonZeroU64};

/// Retired deployment keys remain accepted; commercial policy is validated at startup.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct BssPricingConfig {
    /// Versioned duration observed when issuing an acceptance.
    pub seller_hold_policy: SellerHoldPolicy,
}

/// A deployment's seller policy; changing it never rewrites issued receipts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SellerHoldPolicy {
    /// Positive immutable policy version named by new-sale requests.
    pub version: NonZeroU64,
    /// Positive duration from the server-issued acceptance instant.
    pub duration_seconds: NonZeroU32,
}
impl Default for SellerHoldPolicy {
    fn default() -> Self {
        Self {
            version: NonZeroU64::MIN,
            duration_seconds: DAY,
        }
    }
}
const DAY: NonZeroU32 = match NonZeroU32::new(86_400) {
    Some(value) => value,
    None => unreachable!(),
};
