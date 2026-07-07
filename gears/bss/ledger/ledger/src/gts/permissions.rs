//! BSS Ledger authorization permissions catalog.
//!
//! Declares every ledger-grantable permission as an [`AuthzPermissionV1`] GTS
//! instance via [`gts_instance!`]. Each invocation submits an
//! `InventoryInstance` entry; `types-registry::init()` aggregates and validates
//! them at startup — no registration code in [`crate::module`].
//!
//! `resource_type` values are the authz labels from [`crate::authz`] — the same
//! strings the service paths pass to `PolicyEnforcer` at enforce time, so the
//! catalog and the enforcement path share one source of truth. All labels live
//! OUTSIDE `gts.cf.resources.*`, so only an explicit billing role covers them
//! (the `billing-setup` role grants `provision` + `read` on `ledger` + `close`
//! on `fiscal_period`). Each label is a real object (a noun), never an authz tier.
//!
//! Instance id layout (instance suffix needs ≥5 dot-separated tokens):
//! `gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.<entity>_<action>.v1`.

// The expected-id string literals (here and in the test below) trip DE0901
// (`gts_string_pattern`, which hardcodes the allowed vendor set); they are
// legitimate catalog literals. Suppress file-wide, mirroring
// `rms/src/gts/permissions.rs`.
#![allow(unknown_lints)]
#![allow(de0901_gts_string_pattern)]

use toolkit_gts::{AuthzPermissionV1, gts_instance};

use crate::authz::{actions, labels};

// ── entry — data plane (post / reverse / read) ───────────────────────────────

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.entry_post.v1",
        resource_type: labels::ENTRY.to_owned(),
        action: actions::POST.to_owned(),
        display_name: "Post a ledger entry".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.entry_reverse.v1",
        resource_type: labels::ENTRY.to_owned(),
        action: actions::REVERSE.to_owned(),
        display_name: "Reverse a ledger entry".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.entry_approve.v1",
        resource_type: labels::ENTRY.to_owned(),
        action: actions::APPROVE.to_owned(),
        display_name: "Approve a dual-control ledger entry".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.entry_read.v1",
        resource_type: labels::ENTRY.to_owned(),
        action: actions::READ.to_owned(),
        display_name: "Read ledger balances".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.entry_annotate.v1",
        resource_type: labels::ENTRY.to_owned(),
        action: actions::ANNOTATE.to_owned(),
        display_name: "Annotate an entry with a controlled non-financial note".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.entry_audit_read.v1",
        resource_type: labels::ENTRY.to_owned(),
        action: actions::AUDIT_READ.to_owned(),
        display_name: "Read the secured audit surface (incl. cross-tenant elevation)".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.entry_erase.v1",
        resource_type: labels::ENTRY.to_owned(),
        action: actions::ERASE.to_owned(),
        display_name: "Erase a payer's PII (GDPR right-to-erasure)".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.entry_reidentify.v1",
        resource_type: labels::ENTRY.to_owned(),
        action: actions::REIDENTIFY.to_owned(),
        display_name: "Re-identify a payer's PII reference (forensic)".to_owned(),
    }
}

// ── ledger — control plane: seed + read a seller's ledger ────────────────────

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.ledger_provision.v1",
        resource_type: labels::LEDGER.to_owned(),
        action: actions::PROVISION.to_owned(),
        display_name: "Provision a ledger".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.ledger_read.v1",
        resource_type: labels::LEDGER.to_owned(),
        action: actions::READ.to_owned(),
        display_name: "Read a ledger's chart of accounts".to_owned(),
    }
}

// ── fiscal_period — control plane: close a period ────────────────────────────

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.fiscal_period_close.v1",
        resource_type: labels::FISCAL_PERIOD.to_owned(),
        action: actions::CLOSE.to_owned(),
        display_name: "Close a fiscal period".to_owned(),
    }
}

// ── payment — data plane: settle / allocate + read allocations ───────────────

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.payment_write.v1",
        resource_type: labels::PAYMENT.to_owned(),
        action: actions::WRITE.to_owned(),
        display_name: "Settle or allocate a payment".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.payment_read.v1",
        resource_type: labels::PAYMENT.to_owned(),
        action: actions::READ.to_owned(),
        display_name: "Read payment allocations / unallocated".to_owned(),
    }
}

// ── credit_application — data plane: grant / apply reusable credit ───────────

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.credit_application_write.v1",
        resource_type: labels::CREDIT_APPLICATION.to_owned(),
        action: actions::WRITE.to_owned(),
        display_name: "Grant or apply reusable credit".to_owned(),
    }
}

// ── dispute — data plane: record a chargeback dispute phase ──────────────────

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.dispute_write.v1",
        resource_type: labels::DISPUTE.to_owned(),
        action: actions::WRITE.to_owned(),
        display_name: "Record a chargeback dispute phase".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.dispute_read.v1",
        resource_type: labels::DISPUTE.to_owned(),
        action: actions::READ.to_owned(),
        display_name: "Read / list chargeback disputes".to_owned(),
    }
}

// ── dual_control_policy — config plane: read / write the threshold policy ─────

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.dual_control_policy_write.v1",
        resource_type: labels::DUAL_CONTROL_POLICY.to_owned(),
        action: actions::WRITE.to_owned(),
        display_name: "Configure dual-control thresholds (D2/A6/TTL)".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.dual_control_policy_read.v1",
        resource_type: labels::DUAL_CONTROL_POLICY.to_owned(),
        action: actions::READ.to_owned(),
        display_name: "Read the effective dual-control policy".to_owned(),
    }
}

// ── ledger config plane: read / write tenant settings (posting policy + FX mode) ─

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.config_write.v1",
        resource_type: labels::LEDGER_CONFIG.to_owned(),
        action: actions::WRITE.to_owned(),
        display_name: "Configure ledger tenant settings (posting policy, FX revaluation mode)"
            .to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.config_read.v1",
        resource_type: labels::LEDGER_CONFIG.to_owned(),
        action: actions::READ.to_owned(),
        display_name: "Read the effective ledger tenant settings".to_owned(),
    }
}

// ── recognition — revenue plane: trigger runs / change schedules + read ──────

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.recognition_write.v1",
        resource_type: labels::RECOGNITION.to_owned(),
        action: actions::WRITE.to_owned(),
        display_name: "Trigger a recognition run or change a schedule".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.recognition_read.v1",
        resource_type: labels::RECOGNITION.to_owned(),
        action: actions::READ.to_owned(),
        display_name: "Read recognition runs / schedules / disaggregation".to_owned(),
    }
}

// ── reconciliation — Revenue Assurance: read the queue / trigger a recon run ──

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.reconciliation_read.v1",
        resource_type: labels::RECONCILIATION.to_owned(),
        action: actions::READ.to_owned(),
        display_name: "Read the exception queue / reconciliation runs".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.reconciliation_run.v1",
        resource_type: labels::RECONCILIATION.to_owned(),
        action: actions::RUN.to_owned(),
        display_name: "Trigger a reconciliation check".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.bss.ledger.reconciliation_resolve.v1",
        resource_type: labels::RECONCILIATION.to_owned(),
        action: actions::RESOLVE.to_owned(),
        display_name: "Resolve / acknowledge / approve a close-blocking exception".to_owned(),
    }
}

#[cfg(test)]
#[path = "permissions_tests.rs"]
mod tests;
