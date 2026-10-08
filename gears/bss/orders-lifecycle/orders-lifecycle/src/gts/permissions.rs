//! Registered Orders permission catalog (08 §3.5/§4.3). Registration grants no authority.
//!
//! The same constants feed the `AuthzPermissionV1` instances, the PEP resource descriptors and
//! every enforcement call, so a catalog/route drift fails the census tests.
use authz_resolver_sdk::pep::ResourceType;
use toolkit_gts::gts_id;

/// Registered authorization resource labels (08 §3.5), separate from event/payload types.
pub mod labels {
    use toolkit_gts::gts_id;
    pub const ORDER: &str = gts_id!("cf.bss.orders.order.v1~");
    pub const ACCEPTANCE: &str = gts_id!("cf.bss.orders.acceptance.v1~");
    pub const AUDIT: &str = gts_id!("cf.bss.orders.audit.v1~");
    pub const AUDIT_UNRESOLVED: &str = gts_id!("cf.bss.orders.audit_unresolved.v1~");
}

/// Named PDP properties. The aggregate is `no_tenant`: no business axis is an owner tenant.
pub mod properties {
    pub const RESOURCE_TENANT_ID: &str = "resource_tenant_id";
    pub const SELLER_TENANT_ID: &str = "seller_tenant_id";
    pub const PAYER_TENANT_ID: &str = "payer_tenant_id";
    /// Standard resource ID, resolved to the order primary key.
    pub const ID: &str = toolkit_security::pep_properties::RESOURCE_ID;
    /// Stored refusal subject tenant; the only `audit-unresolved` scope property.
    pub const SUBJECT_TENANT_ID: &str = "subject_tenant_id";
}

const ORDER_PROPERTIES: &[&str] = &[
    properties::RESOURCE_TENANT_ID,
    properties::SELLER_TENANT_ID,
    properties::PAYER_TENANT_ID,
    properties::ID,
];

/// Aggregate authorization uses named business axes, never `owner_tenant_id`.
pub static ORDER: ResourceType = ResourceType::from_static(labels::ORDER, ORDER_PROPERTIES);
/// Acceptance recording is decided against the target order's current axes.
pub static ACCEPTANCE: ResourceType =
    ResourceType::from_static(labels::ACCEPTANCE, ORDER_PROPERTIES);
/// Resolved audit disclosure follows current-parent order scope (D-104).
pub static AUDIT: ResourceType = ResourceType::from_static(labels::AUDIT, ORDER_PROPERTIES);
/// Unresolved refusal rows are scoped by stored subject tenant only.
pub static AUDIT_UNRESOLVED: ResourceType =
    ResourceType::from_static(labels::AUDIT_UNRESOLVED, &[properties::SUBJECT_TENANT_ID]);

/// One independently grantable resource/action pair.
#[derive(Debug, Clone, Copy)]
pub struct Permission {
    pub id: &'static str,
    pub resource: &'static str,
    pub logical_resource: &'static str,
    pub logical_action: &'static str,
    pub action: &'static str,
}

/// Includes operational unresolved-audit access, which has no additional REST endpoint.
pub const PERMISSIONS: &[Permission] = &[
    Permission {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.orders.order_create.v1"),
        resource: labels::ORDER,
        logical_resource: "order",
        logical_action: "create",
        action: "create",
    },
    Permission {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.orders.order_write.v1"),
        resource: labels::ORDER,
        logical_resource: "order",
        logical_action: "write",
        action: "write",
    },
    Permission {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.orders.order_submit.v1"),
        resource: labels::ORDER,
        logical_resource: "order",
        logical_action: "submit",
        action: "submit",
    },
    Permission {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.orders.order_amend.v1"),
        resource: labels::ORDER,
        logical_resource: "order",
        logical_action: "amend",
        action: "amend",
    },
    Permission {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.orders.order_edit.v1"),
        resource: labels::ORDER,
        logical_resource: "order",
        logical_action: "edit",
        action: "edit",
    },
    Permission {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.orders.order_preview.v1"),
        resource: labels::ORDER,
        logical_resource: "order",
        logical_action: "preview",
        action: "preview",
    },
    Permission {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.orders.order_cancel.v1"),
        resource: labels::ORDER,
        logical_resource: "order",
        logical_action: "cancel",
        action: "cancel",
    },
    Permission {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.orders.order_hold.v1"),
        resource: labels::ORDER,
        logical_resource: "order",
        logical_action: "hold",
        action: "hold",
    },
    Permission {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.orders.order_resume.v1"),
        resource: labels::ORDER,
        logical_resource: "order",
        logical_action: "resume",
        action: "resume",
    },
    Permission {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.orders.order_read.v1"),
        resource: labels::ORDER,
        logical_resource: "order",
        logical_action: "read",
        action: "read",
    },
    Permission {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.orders.order_approval_reflection.v1"),
        resource: labels::ORDER,
        logical_resource: "order",
        logical_action: "approval-reflection",
        action: "approval_reflection",
    },
    Permission {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.orders.order_begin_fulfillment.v1"),
        resource: labels::ORDER,
        logical_resource: "order",
        logical_action: "begin-fulfillment",
        action: "begin_fulfillment",
    },
    Permission {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.orders.order_spawn_signal.v1"),
        resource: labels::ORDER,
        logical_resource: "order",
        logical_action: "spawn-signal",
        action: "spawn_signal",
    },
    Permission {
        id: gts_id!(
            "cf.toolkit.authz.permission.v1~cf.bss.orders.order_fulfillment_acknowledgement.v1"
        ),
        resource: labels::ORDER,
        logical_resource: "order",
        logical_action: "fulfillment-acknowledgement",
        action: "fulfillment_acknowledgement",
    },
    Permission {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.orders.order_workflow_cancel.v1"),
        resource: labels::ORDER,
        logical_resource: "order",
        logical_action: "workflow-cancel",
        action: "workflow_cancel",
    },
    Permission {
        id: gts_id!(
            "cf.toolkit.authz.permission.v1~cf.bss.orders.order_force_fail_unreconciled.v1"
        ),
        resource: labels::ORDER,
        logical_resource: "order",
        logical_action: "force-fail-unreconciled",
        action: "force_fail_unreconciled",
    },
    Permission {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.orders.acceptance_record.v1"),
        resource: labels::ACCEPTANCE,
        logical_resource: "acceptance",
        logical_action: "record",
        action: "record",
    },
    Permission {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.orders.audit_read.v1"),
        resource: labels::AUDIT,
        logical_resource: "audit",
        logical_action: "read",
        action: "read",
    },
    Permission {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.orders.audit_unresolved_read.v1"),
        resource: labels::AUDIT_UNRESOLVED,
        logical_resource: "audit-unresolved",
        logical_action: "read",
        action: "read",
    },
];

/// The registered descriptor for a logical resource; `None` for an undeclared resource.
#[must_use]
pub fn resource_type(logical_resource: &str) -> Option<&'static ResourceType> {
    match logical_resource {
        "order" => Some(&ORDER),
        "acceptance" => Some(&ACCEPTANCE),
        "audit" => Some(&AUDIT),
        "audit-unresolved" => Some(&AUDIT_UNRESOLVED),
        _ => None,
    }
}

/// Registered permission for a logical resource/action pair.
#[must_use]
pub fn permission(logical_resource: &str, logical_action: &str) -> Option<&'static Permission> {
    PERMISSIONS
        .iter()
        .find(|p| p.logical_resource == logical_resource && p.logical_action == logical_action)
}
