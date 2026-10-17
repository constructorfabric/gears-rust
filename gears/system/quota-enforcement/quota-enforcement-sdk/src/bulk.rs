//! Bulk Quota CRUD (PRD section 5.2): an envelope of creates, updates, or
//! deactivations of one tenant's Quotas, applied all or nothing under one
//! envelope idempotency key.
//!
//! A caller sends one of the three requests; the gear validates and
//! authorizes every item and hands the plugin one of the three storage
//! envelopes, which it applies in a single transaction. The outcome lists one
//! summary per item in submission order; it is what the envelope's
//! idempotency record stores and what a replay returns verbatim.

use serde::{Deserialize, Serialize};
use toolkit_security::AccessScope;

use crate::models::{
    IdempotencyWrite, LeaseToken, NotificationEvent, QuotaDraft, QuotaId, QuotaPatch, QuotaSpec,
    TenantId,
};

/// Create every listed Quota of one tenant, or none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BulkCreateQuotasRequest {
    /// The one tenant every draft belongs to.
    pub tenant_id: TenantId,
    /// The envelope idempotency key.
    pub idempotency_key: String,
    /// The drafts, in submission order.
    pub items: Vec<BulkCreateItem>,
}

/// One draft of a bulk create.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BulkCreateItem {
    /// Identifies the item in outcomes and errors; no replay of its own.
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// The Quota to create.
    pub spec: QuotaSpec,
}

/// Apply every listed patch to one tenant's Quotas, or none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BulkUpdateQuotasRequest {
    /// The one tenant every Quota belongs to.
    pub tenant_id: TenantId,
    /// The envelope idempotency key.
    pub idempotency_key: String,
    /// The patches, in submission order.
    pub items: Vec<BulkUpdateItem>,
}

/// One patch of a bulk update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BulkUpdateItem {
    /// Identifies the item in outcomes and errors; no replay of its own.
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// The Quota to patch.
    pub quota_id: QuotaId,
    /// The non-breaking patch.
    pub patch: QuotaPatch,
}

/// Deactivate every listed Quota of one tenant, or none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BulkDeactivateQuotasRequest {
    /// The one tenant every Quota belongs to.
    pub tenant_id: TenantId,
    /// The envelope idempotency key.
    pub idempotency_key: String,
    /// The Quotas, in submission order.
    pub items: Vec<BulkDeactivateItem>,
}

/// One Quota of a bulk deactivate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BulkDeactivateItem {
    /// Identifies the item in outcomes and errors; no replay of its own.
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// The Quota to deactivate.
    pub quota_id: QuotaId,
}

/// A committed bulk create: one entry per item, in submission order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkCreated {
    /// The created Quotas.
    pub items: Vec<BulkCreatedItem>,
}

/// One created Quota.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkCreatedItem {
    /// Position of the item in the request.
    pub index: usize,
    /// The item's key, when it carried one.
    pub idempotency_key: Option<String>,
    /// The server-assigned identifier.
    pub quota_id: QuotaId,
}

/// A committed bulk update: one entry per item, in submission order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkUpdated {
    /// The patched Quotas.
    pub items: Vec<BulkUpdatedItem>,
}

/// One patched Quota.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkUpdatedItem {
    /// Position of the item in the request.
    pub index: usize,
    /// The item's key, when it carried one.
    pub idempotency_key: Option<String>,
    /// The patched Quota.
    pub quota_id: QuotaId,
    /// Its `record_version` as the envelope committed it.
    pub record_version: u32,
}

/// A committed bulk deactivate: one entry per item, in submission order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkDeactivated {
    /// The deactivated Quotas.
    pub items: Vec<BulkDeactivatedItem>,
}

/// One deactivated Quota.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkDeactivatedItem {
    /// Position of the item in the request.
    pub index: usize,
    /// The item's key, when it carried one.
    pub idempotency_key: Option<String>,
    /// The deactivated Quota.
    pub quota_id: QuotaId,
    /// The leases this item's cascade resolved. A lease spanning several
    /// Quotas of the envelope is resolved, and listed, by the first of them in
    /// submission order.
    pub resolved_leases: Vec<LeaseToken>,
}

/// What a bulk envelope's idempotency record stores: the outcome under a
/// schema version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkRecord<T> {
    /// Schema version of this record.
    #[serde(rename = "__version")]
    pub version: u32,
    /// The committed outcome, returned verbatim on replay.
    pub outcome: T,
}

impl<T> BulkRecord<T> {
    /// The current schema version.
    pub const VERSION: u32 = 1;

    /// A record of `outcome` under the current version.
    #[must_use]
    pub const fn new(outcome: T) -> Self {
        Self {
            version: Self::VERSION,
            outcome,
        }
    }
}

/// A bulk create as storage applies it.
#[derive(Debug, Clone)]
pub struct BulkCreateEnvelope {
    /// The one tenant of the envelope; every draft names it.
    pub tenant_id: TenantId,
    /// The envelope's idempotency scope and payload digest.
    pub idempotency: IdempotencyWrite,
    /// The drafts, in submission order.
    pub items: Vec<BulkCreateEntry>,
}

/// One validated draft of a bulk create.
#[derive(Debug, Clone)]
pub struct BulkCreateEntry {
    /// The item's key, echoed in the outcome.
    pub idempotency_key: Option<String>,
    /// What the PDP authorized for this item.
    pub scope: AccessScope,
    /// The validated draft.
    pub draft: QuotaDraft,
    /// Its `quota-changed` event; storage fills in the assigned id.
    pub events: Vec<NotificationEvent>,
}

/// A bulk update as storage applies it.
#[derive(Debug, Clone)]
pub struct BulkUpdateEnvelope {
    /// The one tenant of the envelope; a Quota of another tenant is not found.
    pub tenant_id: TenantId,
    /// The envelope's idempotency scope and payload digest.
    pub idempotency: IdempotencyWrite,
    /// The patches, in submission order.
    pub items: Vec<BulkUpdateEntry>,
}

/// One validated patch of a bulk update.
#[derive(Debug, Clone)]
pub struct BulkUpdateEntry {
    /// The item's key, echoed in the outcome.
    pub idempotency_key: Option<String>,
    /// What the PDP authorized for this item.
    pub scope: AccessScope,
    /// The Quota to patch.
    pub quota_id: QuotaId,
    /// The validated patch, `constraint_contract` set with `metadata`.
    pub patch: QuotaPatch,
    /// Its `quota-changed` event.
    pub events: Vec<NotificationEvent>,
}

/// A bulk deactivate as storage applies it.
#[derive(Debug, Clone)]
pub struct BulkDeactivateEnvelope {
    /// The one tenant of the envelope; a Quota of another tenant is not found.
    pub tenant_id: TenantId,
    /// The envelope's idempotency scope and payload digest.
    pub idempotency: IdempotencyWrite,
    /// The Quotas, in submission order.
    pub items: Vec<BulkDeactivateEntry>,
}

/// One Quota of a bulk deactivate.
#[derive(Debug, Clone)]
pub struct BulkDeactivateEntry {
    /// The item's key, echoed in the outcome.
    pub idempotency_key: Option<String>,
    /// What the PDP authorized for this item.
    pub scope: AccessScope,
    /// The Quota to deactivate.
    pub quota_id: QuotaId,
    /// Its `quota-changed` event; storage adds one
    /// `lease-resolved-by-deactivation` event per lease it resolves.
    pub events: Vec<NotificationEvent>,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "bulk_tests.rs"]
mod bulk_tests;
