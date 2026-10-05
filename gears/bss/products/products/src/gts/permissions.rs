//! The seventeen grantable Products permissions, registered as GTS instances. A derived usage type
//! has one, `author` (P-D-231): its reads are `sku:read` (O-3).
#![allow(unknown_lints)]
#![allow(de0901_gts_string_pattern)]
use crate::authz::{actions, labels};
use toolkit_gts::{AuthzPermissionV1, gts_instance};

gts_instance! {
    #[gts_static(SKU_READ)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.products.sku_read.v1"),
        resource_type:labels::SKU.to_owned(),
        action:actions::READ.to_owned(),
        display_name:"Read sku".to_owned(),
    }
}

gts_instance! {
    #[gts_static(SKU_AUTHOR)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.products.sku_author.v1"),
        resource_type:labels::SKU.to_owned(),
        action:actions::AUTHOR.to_owned(),
        display_name:"Author sku".to_owned(),
    }
}

gts_instance! {
    #[gts_static(SKU_SUBMIT)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.products.sku_submit.v1"),
        resource_type:labels::SKU.to_owned(),
        action:actions::SUBMIT.to_owned(),
        display_name:"Submit sku".to_owned(),
    }
}

gts_instance! {
    #[gts_static(SKU_APPROVE)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.products.sku_approve.v1"),
        resource_type:labels::SKU.to_owned(),
        action:actions::APPROVE.to_owned(),
        display_name:"Approve sku".to_owned(),
    }
}

gts_instance! {
    #[gts_static(SKU_SETTINGS)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.products.sku_settings.v1"),
        resource_type:labels::SKU.to_owned(),
        action:actions::SETTINGS.to_owned(),
        display_name:"Settings sku".to_owned(),
    }
}

gts_instance! {
    #[gts_static(SKU_REFERENCE)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.products.sku_reference.v1"),
        resource_type:labels::SKU.to_owned(),
        action:actions::REFERENCE.to_owned(),
        display_name:"Reference sku".to_owned(),
    }
}

gts_instance! {
    #[gts_static(CATEGORY_READ)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.products.category_read.v1"),
        resource_type:labels::CATEGORY.to_owned(),
        action:actions::READ.to_owned(),
        display_name:"Read category".to_owned(),
    }
}

gts_instance! {
    #[gts_static(CATEGORY_AUTHOR)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.products.category_author.v1"),
        resource_type:labels::CATEGORY.to_owned(),
        action:actions::AUTHOR.to_owned(),
        display_name:"Author category".to_owned(),
    }
}

gts_instance! {
    #[gts_static(CATEGORY_SUBMIT)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.products.category_submit.v1"),
        resource_type:labels::CATEGORY.to_owned(),
        action:actions::SUBMIT.to_owned(),
        display_name:"Submit category".to_owned(),
    }
}

gts_instance! {
    #[gts_static(CATEGORY_APPROVE)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.products.category_approve.v1"),
        resource_type:labels::CATEGORY.to_owned(),
        action:actions::APPROVE.to_owned(),
        display_name:"Approve category".to_owned(),
    }
}

gts_instance! {
    #[gts_static(CATEGORY_SETTINGS)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.products.category_settings.v1"),
        resource_type:labels::CATEGORY.to_owned(),
        action:actions::SETTINGS.to_owned(),
        display_name:"Settings category".to_owned(),
    }
}

gts_instance! {
    #[gts_static(APPROVAL_UNIT_READ)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.products.approval_unit_read.v1"),
        resource_type:labels::APPROVAL_UNIT.to_owned(),
        action:actions::READ.to_owned(),
        display_name:"Read approval unit".to_owned(),
    }
}

gts_instance! {
    #[gts_static(APPROVAL_UNIT_AUTHOR)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.products.approval_unit_author.v1"),
        resource_type:labels::APPROVAL_UNIT.to_owned(),
        action:actions::AUTHOR.to_owned(),
        display_name:"Author approval unit".to_owned(),
    }
}

gts_instance! {
    #[gts_static(APPROVAL_UNIT_SUBMIT)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.products.approval_unit_submit.v1"),
        resource_type:labels::APPROVAL_UNIT.to_owned(),
        action:actions::SUBMIT.to_owned(),
        display_name:"Submit approval unit".to_owned(),
    }
}

gts_instance! {
    #[gts_static(APPROVAL_UNIT_APPROVE)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.products.approval_unit_approve.v1"),
        resource_type:labels::APPROVAL_UNIT.to_owned(),
        action:actions::APPROVE.to_owned(),
        display_name:"Approve approval unit".to_owned(),
    }
}

gts_instance! {
    #[gts_static(APPROVAL_UNIT_SETTINGS)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.products.approval_unit_settings.v1"),
        resource_type:labels::APPROVAL_UNIT.to_owned(),
        action:actions::SETTINGS.to_owned(),
        display_name:"Settings approval unit".to_owned(),
    }
}

gts_instance! {
    #[gts_static(DERIVED_USAGE_TYPE_AUTHOR)]
    AuthzPermissionV1 {
        id:gts_id!("cf.toolkit.authz.permission.v1~cf.bss.products.derived_usage_type_author.v1"),
        resource_type:labels::DERIVED_USAGE_TYPE.to_owned(),
        action:actions::AUTHOR.to_owned(),
        display_name:"Author derived usage type".to_owned(),
    }
}

/// Enumerate the exact typed permissions registered by this catalog.
#[must_use]
pub fn all() -> Vec<&'static AuthzPermissionV1> {
    vec![
        &SKU_READ,
        &SKU_AUTHOR,
        &SKU_SUBMIT,
        &SKU_APPROVE,
        &SKU_SETTINGS,
        &SKU_REFERENCE,
        &CATEGORY_READ,
        &CATEGORY_AUTHOR,
        &CATEGORY_SUBMIT,
        &CATEGORY_APPROVE,
        &CATEGORY_SETTINGS,
        &APPROVAL_UNIT_READ,
        &APPROVAL_UNIT_AUTHOR,
        &APPROVAL_UNIT_SUBMIT,
        &APPROVAL_UNIT_APPROVE,
        &APPROVAL_UNIT_SETTINGS,
        &DERIVED_USAGE_TYPE_AUTHOR,
    ]
}
