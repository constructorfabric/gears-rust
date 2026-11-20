//! The gear's authorization permissions catalogue.
//!
//! Declares every permission the gear can be granted as an
//! [`AuthzPermissionV1`] GTS instance via [`gts_instance!`]. Each invocation
//! submits an `InventoryInstance` entry to this crate's collector, which the
//! gear publishes itself (`gts_publication(...)`, admitted before any `init`),
//! and to the process-wide inventory an embedded `types-registry::init()` still
//! aggregates. The entries are registered in `types-registry` as instances of
//! `gts.cf.toolkit.authz.permission.v1~`: the list a role is composed from.
//!
//! `resource_type` and `action` come from [`Permission`], the value the
//! handlers pass to the policy enforcer, so the catalogue speaks the
//! enforcer's vocabulary. A test pins one instance to each [`Permission`]
//! variant; the instances are still written out here, one per variant, because
//! the macro declares them one at a time.
//!
//! Instance id layout:
//! `gts.cf.toolkit.authz.permission.v1~cf.core.settings.<resource>_<action>.v1`.

use toolkit_gts::AuthzPermissionV1;
use toolkit_gts::local::gts_instance;

use crate::api::authz::Permission;

// ---- category (gts.cf.core.settings.category.v1~) ----

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.settings.category_read.v1"),
        resource_type: Permission::CategoryRead.resource().name().to_owned(),
        action: Permission::CategoryRead.action().to_owned(),
        display_name: "Read settings categories".to_owned(),
    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.settings.category_create.v1"),
        resource_type: Permission::CategoryCreate.resource().name().to_owned(),
        action: Permission::CategoryCreate.action().to_owned(),
        display_name: "Create a settings category".to_owned(),
    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.settings.category_update.v1"),
        resource_type: Permission::CategoryUpdate.resource().name().to_owned(),
        action: Permission::CategoryUpdate.action().to_owned(),
        display_name: "Update a settings category".to_owned(),
    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.settings.category_delete.v1"),
        resource_type: Permission::CategoryDelete.resource().name().to_owned(),
        action: Permission::CategoryDelete.action().to_owned(),
        display_name: "Delete a settings category".to_owned(),
    }
}

// ---- declaration (gts.cf.core.settings.declaration.v1~) ----

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.settings.declaration_read.v1"),
        resource_type: Permission::DeclarationRead.resource().name().to_owned(),
        action: Permission::DeclarationRead.action().to_owned(),
        display_name: "Read setting declarations".to_owned(),
    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.settings.declaration_create.v1"),
        resource_type: Permission::DeclarationCreate.resource().name().to_owned(),
        action: Permission::DeclarationCreate.action().to_owned(),
        display_name: "Create a setting declaration".to_owned(),
    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.settings.declaration_update.v1"),
        resource_type: Permission::DeclarationUpdate.resource().name().to_owned(),
        action: Permission::DeclarationUpdate.action().to_owned(),
        display_name: "Update a setting declaration".to_owned(),
    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.settings.declaration_delete.v1"),
        resource_type: Permission::DeclarationDelete.resource().name().to_owned(),
        action: Permission::DeclarationDelete.action().to_owned(),
        display_name: "Retire or purge a setting declaration".to_owned(),
    }
}

// ---- value (gts.cf.core.settings.value.v1~) ----

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.settings.value_read.v1"),
        resource_type: Permission::ValueRead.resource().name().to_owned(),
        action: Permission::ValueRead.action().to_owned(),
        display_name: "Read setting values, including secrets resolved for services".to_owned(),
    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.settings.value_read_unmasked.v1"),
        resource_type: Permission::ValueReadUnmasked.resource().name().to_owned(),
        action: Permission::ValueReadUnmasked.action().to_owned(),
        display_name: "Read setting values unmasked".to_owned(),
    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.settings.value_write.v1"),
        resource_type: Permission::ValueWrite.resource().name().to_owned(),
        action: Permission::ValueWrite.action().to_owned(),
        display_name: "Write setting values".to_owned(),
    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.settings.value_delegate.v1"),
        resource_type: Permission::ValueDelegate.resource().name().to_owned(),
        action: Permission::ValueDelegate.action().to_owned(),
        display_name: "Restrict descendants' access to a setting".to_owned(),
    }
}
