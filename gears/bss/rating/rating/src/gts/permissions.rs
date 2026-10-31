//! The grantable Rating permissions, one per `(resource type, action)` of the catalog (DESIGN
//! §4.8), registered as GTS instances.
#![allow(
    unknown_lints,
    reason = "de0901 is a dylint lint that rustc does not know"
)]
#![allow(
    de0901_gts_string_pattern,
    reason = "every gts_instance! here spells a permission id, which the lint cannot see through"
)]
use crate::authz::{actions, labels};
use toolkit_gts::{AuthzPermissionV1, gts_instance};

gts_instance! {
    #[gts_static(USAGE_READ)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.usage_read.v1"),
        resource_type:labels::USAGE.to_owned(),
        action:actions::READ.to_owned(),
        display_name:"Read usage".to_owned(),
    }
}

gts_instance! {
    #[gts_static(USAGE_RETRY)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.usage_retry.v1"),
        resource_type:labels::USAGE.to_owned(),
        action:actions::RETRY.to_owned(),
        display_name:"Retry usage".to_owned(),
    }
}

gts_instance! {
    #[gts_static(USAGE_RELEASE)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.usage_release.v1"),
        resource_type:labels::USAGE.to_owned(),
        action:actions::RELEASE.to_owned(),
        display_name:"Release usage".to_owned(),
    }
}

gts_instance! {
    #[gts_static(RESULT_READ)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.result_read.v1"),
        resource_type:labels::RESULT.to_owned(),
        action:actions::READ.to_owned(),
        display_name:"Read result".to_owned(),
    }
}

gts_instance! {
    #[gts_static(RERATE_READ)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.rerate_read.v1"),
        resource_type:labels::RERATE.to_owned(),
        action:actions::READ.to_owned(),
        display_name:"Read rerate".to_owned(),
    }
}

gts_instance! {
    #[gts_static(RERATE_EXECUTE)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.rerate_execute.v1"),
        resource_type:labels::RERATE.to_owned(),
        action:actions::EXECUTE.to_owned(),
        display_name:"Execute rerate".to_owned(),
    }
}

gts_instance! {
    #[gts_static(EVALUATION_EVALUATE)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.evaluation_evaluate.v1"),
        resource_type:labels::EVALUATION.to_owned(),
        action:actions::EVALUATE.to_owned(),
        display_name:"Evaluate evaluation".to_owned(),
    }
}

gts_instance! {
    #[gts_static(FINALIZATION_POLICY_READ)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.finalization_policy_read.v1"),
        resource_type:labels::FINALIZATION_POLICY.to_owned(),
        action:actions::READ.to_owned(),
        display_name:"Read finalization policy".to_owned(),
    }
}

gts_instance! {
    #[gts_static(FINALIZATION_POLICY_WRITE)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.finalization_policy_write.v1"),
        resource_type:labels::FINALIZATION_POLICY.to_owned(),
        action:actions::WRITE.to_owned(),
        display_name:"Write finalization policy".to_owned(),
    }
}

gts_instance! {
    #[gts_static(FINALIZATION_POLICY_ACCEPT_DELAY_ONLY)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.finalization_policy_accept_delay_only.v1"),
        resource_type:labels::FINALIZATION_POLICY.to_owned(),
        action:actions::ACCEPT_DELAY_ONLY.to_owned(),
        display_name:"Accept delay only finalization policy".to_owned(),
    }
}

gts_instance! {
    #[gts_static(PLATFORM_FINALIZATION_POLICY_READ)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.platform_finalization_policy_read.v1"),
        resource_type:labels::PLATFORM_FINALIZATION_POLICY.to_owned(),
        action:actions::READ.to_owned(),
        display_name:"Read platform finalization policy".to_owned(),
    }
}

gts_instance! {
    #[gts_static(PLATFORM_FINALIZATION_POLICY_WRITE)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.platform_finalization_policy_write.v1"),
        resource_type:labels::PLATFORM_FINALIZATION_POLICY.to_owned(),
        action:actions::WRITE.to_owned(),
        display_name:"Write platform finalization policy".to_owned(),
    }
}

gts_instance! {
    #[gts_static(PLATFORM_FINALIZATION_POLICY_ACCEPT_DELAY_ONLY)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.platform_finalization_policy_accept_delay_only.v1"),
        resource_type:labels::PLATFORM_FINALIZATION_POLICY.to_owned(),
        action:actions::ACCEPT_DELAY_ONLY.to_owned(),
        display_name:"Accept delay only platform finalization policy".to_owned(),
    }
}

gts_instance! {
    #[gts_static(SOURCE_LOSS_RESOLVE)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.source_loss_resolve.v1"),
        resource_type:labels::SOURCE_LOSS.to_owned(),
        action:actions::RESOLVE.to_owned(),
        display_name:"Resolve source loss".to_owned(),
    }
}

gts_instance! {
    #[gts_static(SOURCE_LOSS_APPROVE)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.source_loss_approve.v1"),
        resource_type:labels::SOURCE_LOSS.to_owned(),
        action:actions::APPROVE.to_owned(),
        display_name:"Approve source loss".to_owned(),
    }
}

gts_instance! {
    #[gts_static(RETENTION_HOLD_READ)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.retention_hold_read.v1"),
        resource_type:labels::RETENTION_HOLD.to_owned(),
        action:actions::READ.to_owned(),
        display_name:"Read retention hold".to_owned(),
    }
}

gts_instance! {
    #[gts_static(RETENTION_HOLD_PLACE)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.retention_hold_place.v1"),
        resource_type:labels::RETENTION_HOLD.to_owned(),
        action:actions::PLACE.to_owned(),
        display_name:"Place retention hold".to_owned(),
    }
}

gts_instance! {
    #[gts_static(RETENTION_HOLD_RELEASE)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.rating.retention_hold_release.v1"),
        resource_type:labels::RETENTION_HOLD.to_owned(),
        action:actions::RELEASE.to_owned(),
        display_name:"Release retention hold".to_owned(),
    }
}

#[cfg(test)]
#[path = "permissions_tests.rs"]
mod tests;
