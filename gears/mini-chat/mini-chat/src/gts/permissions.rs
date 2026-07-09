//! Mini-chat authorization permissions catalog.
//!
//! Declares every permission mini-chat can be granted as a well-known GTS
//! instance of [`AuthzPermissionV1`] via the typed form of [`gts_instance!`].
//! Each call spells out the full `instance_id`
//! (`gts.cf.toolkit.authz.permission.v1~<segment>`) — the macro emits a
//! compile-time assert that the literal's prefix matches
//! `<AuthzPermissionV1 as GtsSchema>::SCHEMA_ID` exactly, so a typo in the
//! prefix is a build error rather than a silent runtime mismatch. Each
//! invocation submits an [`InventoryInstance`] entry to the global
//! inventory collector; `types-registry::init()` picks them up at startup
//! and validates each payload against the `AuthzPermissionV1` schema.
//!
//! `action` values come from `crate::domain::service::actions` — the same
//! constants the PEP sees at `access_scope(...)` time.
//!
//! `resource_type` values are **wildcard patterns** (GTS §3.5) covering
//! the full mini_chat-derived subtree under each `ai_chat` base. At
//! evaluation time the PEP sends a concrete type id (from
//! `crate::domain::service::resources::*.name`); the PDP matches it
//! against these wildcards. This keeps the catalog forward-compatible:
//! if tomorrow someone derives `...~cf.core.mini_chat.chat.v1~vendor.ext.v1~`,
//! the existing permissions still cover it without a catalog edit.
//!
//! The typed struct literal gives compile-time field-name and type
//! checking; the `id` field is auto-injected by the macro.
//!
//! Instance ID layout (level-2, underscore marks the empty namespace slot):
//!
//! ```text
//! gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.<permission_name>.v1
//! ```
//!
//! [`AuthzPermissionV1`]: toolkit_gts::AuthzPermissionV1
//! [`InventoryInstance`]: toolkit_gts::InventoryInstance
//! [`gts_instance!`]: toolkit_gts::gts_instance

use crate::domain::service::actions;
use toolkit_gts::{AuthzPermissionV1, gts_instance};

/// Wildcard `resource_type` for permissions over any mini-chat chat.
///
/// Covers `gts.cf.core.ai_chat.chat.v1~cf.core.mini_chat.chat.v1~` (the
/// concrete type the PEP sends) plus any future derivation under that
/// subtree.
const CHAT_RESOURCE_TYPE_WILDCARD: &str = "gts.cf.core.ai_chat.chat.v1~cf.core.mini_chat.chat.*";

/// Wildcard `resource_type` for permissions over any mini-chat model.
const MODEL_RESOURCE_TYPE_WILDCARD: &str = "gts.cf.core.ai_chat.model.v1~cf.core.mini_chat.model.*";

/// Wildcard `resource_type` for permissions over any mini-chat user-quota.
const USER_QUOTA_RESOURCE_TYPE_WILDCARD: &str =
    "gts.cf.core.ai_chat.user_quota.v1~cf.core.mini_chat.user_quota.*";

// =====================================================================
//                       CHAT resource permissions
//           gts.cf.core.ai_chat.chat.v1~cf.core.mini_chat.chat.v1~
// =====================================================================

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_create.v1",
        resource_type: CHAT_RESOURCE_TYPE_WILDCARD.to_owned(),
        action: actions::CREATE.to_owned(),
        display_name: "Create chat".to_owned(),    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_read.v1",
        resource_type: CHAT_RESOURCE_TYPE_WILDCARD.to_owned(),
        action: actions::READ.to_owned(),
        display_name: "Read chat".to_owned(),    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_list.v1",
        resource_type: CHAT_RESOURCE_TYPE_WILDCARD.to_owned(),
        action: actions::LIST.to_owned(),
        display_name: "List chats".to_owned(),    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_update.v1",
        resource_type: CHAT_RESOURCE_TYPE_WILDCARD.to_owned(),
        action: actions::UPDATE.to_owned(),
        display_name: "Update chat".to_owned(),    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_delete.v1",
        resource_type: CHAT_RESOURCE_TYPE_WILDCARD.to_owned(),
        action: actions::DELETE.to_owned(),
        display_name: "Delete chat".to_owned(),    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_list_messages.v1",
        resource_type: CHAT_RESOURCE_TYPE_WILDCARD.to_owned(),
        action: actions::LIST_MESSAGES.to_owned(),
        display_name: "List chat messages".to_owned(),    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_send_message.v1",
        resource_type: CHAT_RESOURCE_TYPE_WILDCARD.to_owned(),
        action: actions::SEND_MESSAGE.to_owned(),
        display_name: "Send chat message".to_owned(),    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_read_turn.v1",
        resource_type: CHAT_RESOURCE_TYPE_WILDCARD.to_owned(),
        action: actions::READ_TURN.to_owned(),
        display_name: "Read chat turn".to_owned(),    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_retry_turn.v1",
        resource_type: CHAT_RESOURCE_TYPE_WILDCARD.to_owned(),
        action: actions::RETRY_TURN.to_owned(),
        display_name: "Retry chat turn".to_owned(),    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_edit_turn.v1",
        resource_type: CHAT_RESOURCE_TYPE_WILDCARD.to_owned(),
        action: actions::EDIT_TURN.to_owned(),
        display_name: "Edit chat turn".to_owned(),    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_delete_turn.v1",
        resource_type: CHAT_RESOURCE_TYPE_WILDCARD.to_owned(),
        action: actions::DELETE_TURN.to_owned(),
        display_name: "Delete chat turn".to_owned(),    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_upload_attachment.v1",
        resource_type: CHAT_RESOURCE_TYPE_WILDCARD.to_owned(),
        action: actions::UPLOAD_ATTACHMENT.to_owned(),
        display_name: "Upload attachment".to_owned(),    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_read_attachment.v1",
        resource_type: CHAT_RESOURCE_TYPE_WILDCARD.to_owned(),
        action: actions::READ_ATTACHMENT.to_owned(),
        display_name: "Read attachment".to_owned(),    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_delete_attachment.v1",
        resource_type: CHAT_RESOURCE_TYPE_WILDCARD.to_owned(),
        action: actions::DELETE_ATTACHMENT.to_owned(),
        display_name: "Delete attachment".to_owned(),    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_set_reaction.v1",
        resource_type: CHAT_RESOURCE_TYPE_WILDCARD.to_owned(),
        action: actions::SET_REACTION.to_owned(),
        display_name: "Set reaction".to_owned(),    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_delete_reaction.v1",
        resource_type: CHAT_RESOURCE_TYPE_WILDCARD.to_owned(),
        action: actions::DELETE_REACTION.to_owned(),
        display_name: "Delete reaction".to_owned(),    }
}

// =====================================================================
//                       MODEL resource permissions
//          gts.cf.core.ai_chat.model.v1~cf.core.mini_chat.model.v1~
// =====================================================================

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.model_list.v1",
        resource_type: MODEL_RESOURCE_TYPE_WILDCARD.to_owned(),
        action: actions::LIST.to_owned(),
        display_name: "List models".to_owned(),    }
}

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.model_read.v1",
        resource_type: MODEL_RESOURCE_TYPE_WILDCARD.to_owned(),
        action: actions::READ.to_owned(),
        display_name: "Read model".to_owned(),    }
}

// =====================================================================
//                    USER_QUOTA resource permissions
//    gts.cf.core.ai_chat.user_quota.v1~cf.core.mini_chat.user_quota.v1~
// =====================================================================

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.user_quota_read.v1",
        resource_type: USER_QUOTA_RESOURCE_TYPE_WILDCARD.to_owned(),
        action: actions::READ.to_owned(),
        display_name: "Read user quota".to_owned(),    }
}

#[cfg(test)]
mod tests {
    use super::{
        CHAT_RESOURCE_TYPE_WILDCARD, MODEL_RESOURCE_TYPE_WILDCARD,
        USER_QUOTA_RESOURCE_TYPE_WILDCARD, actions,
    };
    use crate::domain::service::resources;
    use toolkit_gts::InventoryInstance;

    #[allow(unknown_lints, de0901_gts_string_pattern)]
    const INSTANCE_PREFIX: &str = "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.";

    /// Expected set of permission instance ids for mini-chat. One entry per
    /// `(resource, action)` tuple the gear exposes at PEP call-sites.
    const EXPECTED_PERMISSION_IDS: &[&str] = &[
        "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_create.v1",
        "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_read.v1",
        "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_list.v1",
        "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_update.v1",
        "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_delete.v1",
        "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_list_messages.v1",
        "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_send_message.v1",
        "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_read_turn.v1",
        "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_retry_turn.v1",
        "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_edit_turn.v1",
        "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_delete_turn.v1",
        "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_upload_attachment.v1",
        "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_read_attachment.v1",
        "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_delete_attachment.v1",
        "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_set_reaction.v1",
        "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_delete_reaction.v1",
        "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.model_list.v1",
        "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.model_read.v1",
        "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.user_quota_read.v1",
    ];

    fn mini_chat_permission_instances() -> Vec<&'static InventoryInstance> {
        inventory::iter::<InventoryInstance>
            .into_iter()
            .filter(|e| e.instance_id.starts_with(INSTANCE_PREFIX))
            .collect()
    }

    /// Catalog integrity tripwire + per-entry well-formedness in a single
    /// pass over the inventory.
    ///
    /// - **Exact-set:** the declared permission ids match `EXPECTED_PERMISSION_IDS`
    ///   one-for-one. A `gts_instance!` dropped, renamed, or added without
    ///   updating the expected list fails here — forcing the change through a
    ///   reviewed, explicit list (the catalog is a security surface).
    /// - **Closed sets:** every entry's `resource_type` is one of the known
    ///   wildcards and its `action` is a `domain::service::actions` constant —
    ///   guards against a raw string literal slipping past the named consts.
    ///
    /// Schema-validity of each payload (required fields, types) is enforced at
    /// server boot when `types-registry` commits readiness over the full
    /// inventory, so it is not re-checked here.
    #[test]
    fn catalog_is_well_formed() {
        let known_wildcards: std::collections::BTreeSet<&'static str> = [
            CHAT_RESOURCE_TYPE_WILDCARD,
            MODEL_RESOURCE_TYPE_WILDCARD,
            USER_QUOTA_RESOURCE_TYPE_WILDCARD,
        ]
        .into_iter()
        .collect();
        let domain_actions: std::collections::BTreeSet<&'static str> = [
            actions::CREATE,
            actions::READ,
            actions::LIST,
            actions::UPDATE,
            actions::DELETE,
            actions::LIST_MESSAGES,
            actions::SEND_MESSAGE,
            actions::READ_TURN,
            actions::RETRY_TURN,
            actions::EDIT_TURN,
            actions::DELETE_TURN,
            actions::UPLOAD_ATTACHMENT,
            actions::READ_ATTACHMENT,
            actions::DELETE_ATTACHMENT,
            actions::SET_REACTION,
            actions::DELETE_REACTION,
        ]
        .into_iter()
        .collect();

        let entries = mini_chat_permission_instances();

        // Exact-set: declared ids == expected ids.
        let actual_ids: std::collections::BTreeSet<&str> =
            entries.iter().map(|e| e.instance_id).collect();
        let expected_ids: std::collections::BTreeSet<&str> =
            EXPECTED_PERMISSION_IDS.iter().copied().collect();
        assert_eq!(
            actual_ids, expected_ids,
            "declared permission ids drifted from EXPECTED_PERMISSION_IDS"
        );

        // Closed sets: resource_type ∈ known wildcards, action ∈ domain actions.
        for entry in &entries {
            let payload = (entry.payload_fn)();
            let rt = payload["resource_type"]
                .as_str()
                .expect("resource_type string");
            assert!(
                known_wildcards.contains(rt),
                "permission {} uses unknown resource_type {:?}; expected one of {:?}",
                entry.instance_id,
                rt,
                known_wildcards
            );
            let action = payload["action"].as_str().expect("action string");
            assert!(
                domain_actions.contains(action),
                "permission {} uses action {:?} not declared in domain::service::actions",
                entry.instance_id,
                action
            );
        }
    }

    #[test]
    fn wildcards_cover_runtime_concrete_resource_types() {
        // GTS §3.5 wildcard semantics: `<prefix>.*` matches anything that
        // starts with `<prefix>.`. At evaluation time the PEP sends a
        // concrete type id (from `resources::*.name`); the PDP matches it
        // against the permission's wildcard. Verify that each of our
        // wildcards actually covers the corresponding runtime concrete.
        fn covers(wildcard: &str, concrete: &str) -> bool {
            wildcard
                .strip_suffix('*')
                .is_some_and(|prefix| concrete.starts_with(prefix))
        }

        for (wildcard, concrete, label) in [
            (CHAT_RESOURCE_TYPE_WILDCARD, resources::CHAT.name(), "CHAT"),
            (
                MODEL_RESOURCE_TYPE_WILDCARD,
                resources::MODEL.name(),
                "MODEL",
            ),
            (
                USER_QUOTA_RESOURCE_TYPE_WILDCARD,
                resources::USER_QUOTA.name(),
                "USER_QUOTA",
            ),
        ] {
            assert!(
                covers(wildcard, concrete),
                "{label} wildcard {wildcard:?} must cover runtime concrete {concrete:?} - otherwise a PDP lookup that sees the concrete type won't match this permission"
            );
        }
    }
}
