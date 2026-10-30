//! GTS types and well-known instances owned by the policy engine: the
//! resource types and the management permission catalog, all registered
//! through the `toolkit-gts` inventory. The engine plugin instance is not
//! here: its vendor and priority come from configuration, so the gear
//! registers it at init.

use gts::{GtsInstanceId, GtsSchema};
use toolkit_gts::gts_type_schema;

/// Resource type of a policy bundle.
pub const BUNDLE_RESOURCE: &str = PolicyBundleV1::TYPE_ID;

/// Resource type of a policy bundle version.
pub const BUNDLE_VERSION_RESOURCE: &str = PolicyBundleVersionV1::TYPE_ID;

/// Resource type of a bundle-to-tenant assignment.
pub const ASSIGNMENT_RESOURCE: &str = PolicyAssignmentV1::TYPE_ID;

/// Type schema of [`BUNDLE_RESOURCE`]. The body is `id`-only: the registry
/// needs the type identifier known (permissions name it, and policies may
/// target it) and nothing from the schema body.
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.core.policy_engine.bundle.v1~"),
    description = "Policy engine bundle: a named set of policy versions, resource type of the management permissions",
    properties = "id",
)]
pub struct PolicyBundleV1 {
    /// Required by the `gts-macros` base-struct contract; inert.
    pub id: GtsInstanceId,
}

/// Type schema of [`BUNDLE_VERSION_RESOURCE`]; `id`-only, as
/// [`PolicyBundleV1`].
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.core.policy_engine.bundle_version.v1~"),
    description = "Policy engine bundle version: one draft, active or superseded content of a bundle",
    properties = "id",
)]
pub struct PolicyBundleVersionV1 {
    /// Required by the `gts-macros` base-struct contract; inert.
    pub id: GtsInstanceId,
}

/// Type schema of [`ASSIGNMENT_RESOURCE`]; `id`-only, as [`PolicyBundleV1`].
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.core.policy_engine.assignment.v1~"),
    description = "Policy engine assignment: a bundle bound to a tenant and its descendants",
    properties = "id",
)]
pub struct PolicyAssignmentV1 {
    /// Required by the `gts-macros` base-struct contract; inert.
    pub id: GtsInstanceId,
}

/// Entrypoint rule every policy document defines: a boolean rule that is
/// `true` when the document denies the operation; `false` or undefined when
/// it does not; any other value is an evaluation error.
pub const POLICY_ENTRYPOINT: &str = "deny";

/// The management permission catalog: each capability is a well-known
/// `AuthzPermissionV1` instance submitted to the `toolkit-gts` inventory; the
/// resource type and action are the exact values the management surface passes
/// to the policy enforcer. No capability implies another.
///
/// Instance id layout: `gts.cf.toolkit.authz.permission.v1~cf.core.policy_engine.<name>.v1`.
pub mod permissions {
    use toolkit_gts::{AuthzPermissionV1, gts_id, gts_instance};

    use super::BUNDLE_RESOURCE;

    /// Resource type every management permission applies to.
    pub const RESOURCE_TYPE: &str = BUNDLE_RESOURCE;

    /// Action names the management surface enforces.
    pub mod actions {
        /// Read policy content.
        pub const READ: &str = "read";
        /// Create and update bundles; create, replace, delete and validate drafts.
        pub const AUTHOR: &str = "author";
        /// Activate versions; assign, update and unassign bundles.
        pub const PUBLISH: &str = "publish";
    }

    /// Permission id: read policy content.
    pub const READ: &str = gts_id!("cf.toolkit.authz.permission.v1~cf.core.policy_engine.read.v1");
    /// Permission id: author bundles and drafts.
    pub const AUTHOR: &str =
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.policy_engine.author.v1");
    /// Permission id: publish versions and assignments.
    pub const PUBLISH: &str =
        gts_id!("cf.toolkit.authz.permission.v1~cf.core.policy_engine.publish.v1");

    /// One management capability: its permission id, resource type and
    /// action, as declared in the catalog.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum Capability {
        /// Read policy content.
        Read,
        /// Author bundles and drafts.
        Author,
        /// Publish versions and assignments.
        Publish,
    }

    impl Capability {
        /// Every capability, in declaration order.
        pub const ALL: [Self; 3] = [Self::Read, Self::Author, Self::Publish];

        /// GTS instance id of the capability's permission.
        #[must_use]
        pub fn permission_id(self) -> &'static str {
            match self {
                Self::Read => READ,
                Self::Author => AUTHOR,
                Self::Publish => PUBLISH,
            }
        }

        /// Action the permission grants.
        #[must_use]
        pub fn action(self) -> &'static str {
            match self {
                Self::Read => actions::READ,
                Self::Author => actions::AUTHOR,
                Self::Publish => actions::PUBLISH,
            }
        }
    }

    gts_instance! {
        AuthzPermissionV1 {
            id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.policy_engine.read.v1"),
            resource_type: RESOURCE_TYPE.to_owned(),
            action: actions::READ.to_owned(),
            display_name: "Read policy content".to_owned(),
        }
    }
    gts_instance! {
        AuthzPermissionV1 {
            id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.policy_engine.author.v1"),
            resource_type: RESOURCE_TYPE.to_owned(),
            action: actions::AUTHOR.to_owned(),
            display_name: "Author policy bundles and drafts".to_owned(),
        }
    }
    gts_instance! {
        AuthzPermissionV1 {
            id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.policy_engine.publish.v1"),
            resource_type: RESOURCE_TYPE.to_owned(),
            action: actions::PUBLISH.to_owned(),
            display_name: "Publish policy versions and assignments".to_owned(),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gts_tests.rs"]
mod gts_tests;
