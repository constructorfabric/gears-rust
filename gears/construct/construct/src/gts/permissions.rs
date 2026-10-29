//! Every permission Construct can be granted, one GTS instance of
//! [`AuthzPermissionV1`] per resource type and action its routes and its
//! in-process client enforce. The resource types and actions are the
//! constants the `PolicyEnforcer` calls pass, so the catalog cannot drift from
//! what is checked.
//!
//! The permissions are the separate ones the PRD's access control names:
//! connectors send records; applications and agents read profiles and manage
//! facts for the tenant's subjects; a subject controls its own settings and
//! facts; the data protection officer exports a subject's data. The tenant
//! grants each to whom it chooses, in the policy engine.
//!
//! Instance id layout: `gts.cf.toolkit.authz.permission.v1~cf.construct._.<permission>.v1`.
//!
//! [`AuthzPermissionV1`]: toolkit_gts::AuthzPermissionV1

use toolkit_gts::{AuthzPermissionV1, gts_instance};

use crate::domain::planner::manage::{self, SUBJECT_FACTS_RESOURCE};
use crate::domain::profile_read::{self, PROFILE_RESOURCE};
use crate::domain::record_intake::{self, RECORD_RESOURCE};
use crate::domain::subject_control;
use crate::domain::subject_settings::{self, SUBJECT_SETTINGS_RESOURCE};

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.construct._.record_send.v1"),
        resource_type: RECORD_RESOURCE.name().to_owned(),
        action: record_intake::actions::SEND.to_owned(),
        display_name: "Send records".to_owned(),
    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.construct._.profile_read.v1"),
        resource_type: PROFILE_RESOURCE.name().to_owned(),
        action: profile_read::actions::READ.to_owned(),
        display_name: "Read profiles".to_owned(),
    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.construct._.facts_manage.v1"),
        resource_type: SUBJECT_FACTS_RESOURCE.name().to_owned(),
        action: manage::actions::MANAGE.to_owned(),
        display_name: "Manage facts".to_owned(),
    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.construct._.subject_settings_get.v1"),
        resource_type: SUBJECT_SETTINGS_RESOURCE.name().to_owned(),
        action: subject_settings::actions::GET.to_owned(),
        display_name: "Read own personalization settings".to_owned(),
    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.construct._.subject_settings_update.v1"),
        resource_type: SUBJECT_SETTINGS_RESOURCE.name().to_owned(),
        action: subject_settings::actions::UPDATE.to_owned(),
        display_name: "Change own personalization settings".to_owned(),
    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.construct._.facts_read.v1"),
        resource_type: SUBJECT_FACTS_RESOURCE.name().to_owned(),
        action: subject_control::actions::READ.to_owned(),
        display_name: "Read own facts".to_owned(),
    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.construct._.facts_delete.v1"),
        resource_type: SUBJECT_FACTS_RESOURCE.name().to_owned(),
        action: subject_control::actions::DELETE.to_owned(),
        display_name: "Delete own facts".to_owned(),
    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.construct._.subject_export.v1"),
        resource_type: SUBJECT_FACTS_RESOURCE.name().to_owned(),
        action: subject_control::actions::EXPORT.to_owned(),
        display_name: "Export a subject's data".to_owned(),
    }
}
