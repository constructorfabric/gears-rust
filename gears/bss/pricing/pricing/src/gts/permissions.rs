//! The nineteen grantable Pricing permissions, registered as GTS instances: exactly the
//! (label, action) pairs the doors enforce (D-526). A grant no door asks for is not offered.
#![allow(
    unknown_lints,
    de0901_gts_string_pattern,
    reason = "the gts_instance! expansion carries the GTS id as a string literal; de0901 is a dylint lint plain rustc does not know"
)]
use crate::authz::{actions, labels};
use toolkit_gts::{AuthzPermissionV1, gts_instance};

gts_instance! {
    #[gts_static(PRICE_BOOK_READ)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.price_book_read.v1"),
        resource_type:labels::PRICE_BOOK.to_owned(),
        action:actions::READ.to_owned(),
        display_name:"Read price book".to_owned(),
    }
}

gts_instance! {
    #[gts_static(PRICE_BOOK_AUTHOR)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.price_book_author.v1"),
        resource_type:labels::PRICE_BOOK.to_owned(),
        action:actions::AUTHOR.to_owned(),
        display_name:"Author price book".to_owned(),
    }
}

gts_instance! {
    #[gts_static(PRICE_BOOK_SUBMIT)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.price_book_submit.v1"),
        resource_type:labels::PRICE_BOOK.to_owned(),
        action:actions::SUBMIT.to_owned(),
        display_name:"Submit price book".to_owned(),
    }
}

gts_instance! {
    #[gts_static(PRICE_BOOK_ENTRY_READ)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.price_book_entry_read.v1"),
        resource_type:labels::PRICE_BOOK_ENTRY.to_owned(),
        action:actions::READ.to_owned(),
        display_name:"Read price book entry".to_owned(),
    }
}

gts_instance! {
    #[gts_static(PRICE_BOOK_ENTRY_AUTHOR)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.price_book_entry_author.v1"),
        resource_type:labels::PRICE_BOOK_ENTRY.to_owned(),
        action:actions::AUTHOR.to_owned(),
        display_name:"Author price book entry".to_owned(),
    }
}

gts_instance! {
    #[gts_static(PRICE_READ)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.price_read.v1"),
        resource_type:labels::PRICE.to_owned(),
        action:actions::READ.to_owned(),
        display_name:"Read price".to_owned(),
    }
}

gts_instance! {
    #[gts_static(PRICE_AUTHOR)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.price_author.v1"),
        resource_type:labels::PRICE.to_owned(),
        action:actions::AUTHOR.to_owned(),
        display_name:"Author price".to_owned(),
    }
}

gts_instance! {
    #[gts_static(PRICE_SUBMIT)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.price_submit.v1"),
        resource_type:labels::PRICE.to_owned(),
        action:actions::SUBMIT.to_owned(),
        display_name:"Submit price".to_owned(),
    }
}

gts_instance! {
    #[gts_static(PLAN_READ)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.plan_read.v1"),
        resource_type:labels::PLAN.to_owned(),
        action:actions::READ.to_owned(),
        display_name:"Read plan".to_owned(),
    }
}

gts_instance! {
    #[gts_static(PLAN_AUTHOR)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.plan_author.v1"),
        resource_type:labels::PLAN.to_owned(),
        action:actions::AUTHOR.to_owned(),
        display_name:"Author plan".to_owned(),
    }
}

gts_instance! {
    #[gts_static(PLAN_SUBMIT)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.plan_submit.v1"),
        resource_type:labels::PLAN.to_owned(),
        action:actions::SUBMIT.to_owned(),
        display_name:"Submit plan".to_owned(),
    }
}

gts_instance! {
    #[gts_static(APPROVAL_UNIT_READ)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.approval_unit_read.v1"),
        resource_type:labels::APPROVAL_UNIT.to_owned(),
        action:actions::READ.to_owned(),
        display_name:"Read approval unit".to_owned(),
    }
}

gts_instance! {
    #[gts_static(APPROVAL_UNIT_APPROVE)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.approval_unit_approve.v1"),
        resource_type:labels::APPROVAL_UNIT.to_owned(),
        action:actions::APPROVE.to_owned(),
        display_name:"Approve approval unit".to_owned(),
    }
}

gts_instance! {
    #[gts_static(APPROVAL_UNIT_SUBMIT)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.approval_unit_submit.v1"),
        resource_type:labels::APPROVAL_UNIT.to_owned(),
        action:actions::SUBMIT.to_owned(),
        display_name:"Submit approval unit".to_owned(),
    }
}

gts_instance! {
    #[gts_static(CONFIG_READ)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.config_read.v1"),
        resource_type:labels::CONFIG.to_owned(),
        action:actions::READ.to_owned(),
        display_name:"Read config".to_owned(),
    }
}

gts_instance! {
    #[gts_static(CONFIG_SETTINGS)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.config_settings.v1"),
        resource_type:labels::CONFIG.to_owned(),
        action:actions::SETTINGS.to_owned(),
        display_name:"Settings config".to_owned(),
    }
}

gts_instance! {
    #[gts_static(ACCEPTANCE_CREATE)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.acceptance_create.v1"),
        resource_type:labels::ACCEPTANCE.to_owned(),
        action:actions::CREATE.to_owned(),
        display_name:"Create acceptance".to_owned(),
    }
}

gts_instance! {
    #[gts_static(ACCEPTANCE_HOLD)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.acceptance_hold.v1"),
        resource_type:labels::ACCEPTANCE.to_owned(),
        action:actions::HOLD.to_owned(),
        display_name:"Hold acceptance".to_owned(),
    }
}

gts_instance! {
    #[gts_static(ACCEPTANCE_READ)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.pricing.acceptance_read.v1"),
        resource_type:labels::ACCEPTANCE.to_owned(),
        action:actions::READ.to_owned(),
        display_name:"Read acceptance".to_owned(),
    }
}

/// Enumerate the exact typed permissions registered by this catalog.
#[must_use]
pub fn all() -> Vec<&'static AuthzPermissionV1> {
    vec![
        &PRICE_BOOK_READ,
        &PRICE_BOOK_AUTHOR,
        &PRICE_BOOK_SUBMIT,
        &PRICE_BOOK_ENTRY_READ,
        &PRICE_BOOK_ENTRY_AUTHOR,
        &PRICE_READ,
        &PRICE_AUTHOR,
        &PRICE_SUBMIT,
        &PLAN_READ,
        &PLAN_AUTHOR,
        &PLAN_SUBMIT,
        &APPROVAL_UNIT_READ,
        &APPROVAL_UNIT_APPROVE,
        &APPROVAL_UNIT_SUBMIT,
        &CONFIG_READ,
        &CONFIG_SETTINGS,
        &ACCEPTANCE_CREATE,
        &ACCEPTANCE_HOLD,
        &ACCEPTANCE_READ,
    ]
}
