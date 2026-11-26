//! GTS definitions owned by quota-enforcement.
//!
//! - The **plugin specs**. Plugin gears register an instance of one spec
//!   (vendor + priority). The gear discovers the active plugin through the
//!   types registry and resolves its scoped `ClientHub` client under the
//!   instance id.
//! - The **resource identifiers** the canonical error envelope carries in
//!   `Problem.context.resource_type` (DESIGN section 3.3, "Error Model").
//!
//! The quota subject and metric types belong to the quota resolver SDK.

use toolkit_gts::{PluginV1, gts_id, gts_type_schema};

/// GTS resource type for Quota records (declarative caps).
pub const QUOTA_RESOURCE: &str = gts_id!("cf.core.qe.quota.v1~");

/// GTS resource type for Quota Resolution Policy records and their versions.
pub const POLICY_RESOURCE: &str = gts_id!("cf.core.qe.policy.v1~");

/// GTS resource type for two-phase capacity leases.
pub const LEASE_RESOURCE: &str = gts_id!("cf.core.qe.lease.v1~");

/// GTS resource type for operation-log records.
pub const OPERATION_RESOURCE: &str = gts_id!("cf.core.qe.operation.v1~");

// ---------------------------------------------------------------------------
// Plugin specs
// ---------------------------------------------------------------------------

/// GTS plugin specification for quota-enforcement storage backends.
///
/// Instance id shape:
/// `gts.cf.toolkit.plugins.plugin.v1~cf.core.qe.storage_plugin.v1~<vendor>.<pkg>.<ns>.<name>.v1`
// @cpt-dod:cpt-cf-quota-enforcement-dod-sdk-contracts:p1
#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.core.qe.storage_plugin.v1~"),
    description = "Quota Enforcement storage plugin specification",
    properties = "",
)]
pub struct QuotaEnforcementStoragePluginSpecV1;

/// GTS plugin specification for notification sinks. Unlike storage, every
/// registered instance of every vendor receives every event.
///
/// Instance id shape:
/// `gts.cf.toolkit.plugins.plugin.v1~cf.core.qe.notification_sink.v1~<vendor>.<pkg>.<ns>.<name>.v1`
#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.core.qe.notification_sink.v1~"),
    description = "Quota Enforcement notification sink specification",
    properties = "",
)]
pub struct QuotaNotificationSinkSpecV1;

// Coordination uses the platform cluster profile rather than a plugin spec.

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gts_tests.rs"]
mod gts_tests;
