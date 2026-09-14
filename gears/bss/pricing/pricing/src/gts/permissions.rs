//! Plan & Price Modeling authorization permissions catalog.
//!
//! Declares every grantable permission as an [`AuthzPermissionV1`] GTS instance
//! via [`gts_instance!`]. Each invocation submits an `InventoryInstance` entry;
//! `types-registry::init()` aggregates and validates them at startup — no
//! registration code in [`crate::module`].
//!
//! `resource_type` values are the authz labels from [`crate::authz`] — the same
//! strings the service paths pass to `PolicyEnforcer` at enforce time, so the
//! catalog and the enforcement path share one source of truth. The role matrix
//! these permissions compose into is normative in `design/05-governance.md`;
//! two of its properties are worth restating where the permissions live:
//! no default role holds both `plan × publish` and `approval × approve`, and
//! the approving role holds `read` on every always-material trigger's subject
//! (D-61) — which is why `price_overlay × read` exists as its own permission
//! rather than riding a plan grant. There is no `historical_import × read`: D-330
//! took that trigger and its flow, so the invariant has one fewer subject to range
//! over.
//!
//! Instance id layout (instance suffix needs >= 5 dot-separated tokens):
//! `gts.cf.toolkit.authz.permission.v1~cf.bss.pricing.<entity>_<action>.v1`.

// The expected-id string literals (here and in the drift test) trip DE0901
// (`gts_string_pattern`, which hardcodes the allowed vendor set); they are
// legitimate catalog literals. Suppress file-wide, mirroring the sibling
// ledger's permission catalog.
#![allow(unknown_lints)]
#![allow(de0901_gts_string_pattern)]

use toolkit_gts::{AuthzPermissionV1, gts_instance};

use crate::authz::{actions, labels};

// -- plan -- the authoring data plane + the consumer read surfaces -----------

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.plan_write.v1"),
        resource_type: labels::PLAN.to_owned(),
        action: actions::WRITE.to_owned(),
        display_name: "Author plan and price draft state".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.plan_publish.v1"),
        resource_type: labels::PLAN.to_owned(),
        action: actions::PUBLISH.to_owned(),
        display_name: "Submit a plan for publish".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.plan_retire.v1"),
        resource_type: labels::PLAN.to_owned(),
        action: actions::RETIRE.to_owned(),
        display_name: "Retire a plan".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.plan_migrate.v1"),
        resource_type: labels::PLAN.to_owned(),
        action: actions::MIGRATE.to_owned(),
        display_name: "Schedule or execute a plan migration".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.plan_read.v1"),
        resource_type: labels::PLAN.to_owned(),
        action: actions::READ.to_owned(),
        display_name: "Read plans, prices and the published read model".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.plan_preview.v1"),
        resource_type: labels::PLAN.to_owned(),
        action: actions::PREVIEW.to_owned(),
        display_name: "Preview the catalog base price".to_owned(),
    }
}

// -- bundle -- composition and rev-share -------------------------------------

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.bundle_write.v1"),
        resource_type: labels::BUNDLE.to_owned(),
        action: actions::WRITE.to_owned(),
        display_name: "Author bundle composition and rev-share".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.bundle_read.v1"),
        resource_type: labels::BUNDLE.to_owned(),
        action: actions::READ.to_owned(),
        display_name: "Read bundle composition".to_owned(),
    }
}

// -- price_overlay -- overlays of every scope --------------------------------

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.price_overlay_write.v1"),
        resource_type: labels::PRICE_OVERLAY.to_owned(),
        action: actions::WRITE.to_owned(),
        display_name: "Author a price overlay".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.price_overlay_read.v1"),
        resource_type: labels::PRICE_OVERLAY.to_owned(),
        action: actions::READ.to_owned(),
        display_name: "Read price overlays".to_owned(),
    }
}

// -- customer_group -- taxonomy and per-payer membership ---------------------

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.customer_group_write.v1"),
        resource_type: labels::CUSTOMER_GROUP.to_owned(),
        action: actions::WRITE.to_owned(),
        display_name: "Author customer groups and membership".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.customer_group_read.v1"),
        resource_type: labels::CUSTOMER_GROUP.to_owned(),
        action: actions::READ.to_owned(),
        display_name: "Read customer groups and membership".to_owned(),
    }
}

// -- approval -- decisions on a governed change ------------------------------

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.approval_approve.v1"),
        resource_type: labels::APPROVAL.to_owned(),
        action: actions::APPROVE.to_owned(),
        display_name: "Approve or reject a governed catalog change".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.approval_read.v1"),
        resource_type: labels::APPROVAL.to_owned(),
        action: actions::READ.to_owned(),
        display_name: "Read approval records and their pinned content".to_owned(),
    }
}

// -- approval_policy -- the threshold policy, itself two-person ---------------

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.approval_policy_write.v1"),
        resource_type: labels::APPROVAL_POLICY.to_owned(),
        action: actions::WRITE.to_owned(),
        display_name: "Author the approval-threshold policy".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.approval_policy_read.v1"),
        resource_type: labels::APPROVAL_POLICY.to_owned(),
        action: actions::READ.to_owned(),
        display_name: "Read the approval-threshold policy".to_owned(),
    }
}

// -- config -- tax-display policy and the tenant taxonomies ------------------

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.config_write.v1"),
        resource_type: labels::CONFIG.to_owned(),
        action: actions::WRITE.to_owned(),
        display_name: "Author tenant catalog configuration".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.config_read.v1"),
        resource_type: labels::CONFIG.to_owned(),
        action: actions::READ.to_owned(),
        display_name: "Read tenant catalog configuration".to_owned(),
    }
}

// `historical_import_write` ("Import governed backdated reference prices") and
// `historical_import_read` ("Read a pending backdated import's row set") stood
// here and are **struck** (D-330). S5 §3 registers no
// `historical_import` resource: a grant nobody can be issued and no route asks
// about confers nothing and withholds nothing, and the label's own strike note in
// `crate::authz::labels` is why re-adding either needs a decision. Deleting them
// is what `every_grantable_label_is_enforced_by_some_route` was asking for all
// along — a later wave answered it by moving three bulk-import routes onto
// the label instead, which is the other way to satisfy a membership check.

// -- audit -- the actor / before-after / approval trail ----------------------

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.audit_read.v1"),
        resource_type: labels::AUDIT.to_owned(),
        action: actions::READ.to_owned(),
        display_name: "Read the catalog audit trail".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.audit_export.v1"),
        resource_type: labels::AUDIT.to_owned(),
        action: actions::EXPORT.to_owned(),
        display_name: "Export the catalog audit trail".to_owned(),
    }
}

#[cfg(test)]
#[path = "permissions_tests.rs"]
mod permissions_tests;
