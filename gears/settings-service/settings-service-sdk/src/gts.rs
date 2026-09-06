// Created: 2026-08-12 by Constructor Tech
//! GTS resource types this gear attributes its errors to.
//!
//! These are the values that reach `NotFound.ctx.resource_type` and its
//! siblings. They are the discriminator the [`crate::SettingsError`] projection
//! uses to tell two otherwise identical `NotFound` outcomes apart: a setting
//! that was never declared, and a secret-backed setting whose credential is not
//! configured at any scope. A consumer that conflates the two hands a
//! placeholder to its backend believing it to be a credential.

/// A setting declaration — the record of what a setting *is*.
/// The abstract base every setting key derives from, as a wire string.
///
/// A setting key is this followed by the setting's own derived type (ADR-002).
/// The schema registered under it is [`setting_type_base_schema`] below.
pub const SETTING_TYPE_BASE: &str = "gts.cf.core.settings.setting_type.v1~";

// @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-9
/// The schema of the abstract base every concrete setting type derives from.
///
/// Registered through the link-time inventory: the submission below happens
/// at link time, and the types registry drains the inventory when *it*
/// initializes — before this gear, which names it in `deps` — so the base exists
/// before any declaration path composes a derived type from it. No call is made
/// from this gear's init, and there is nothing to retry.
///
/// A concrete setting is a type `SETTING_TYPE_BASE<vendor>.<package>.<category>.<name>.vN~`
/// derived from this base, narrowing `payload` to the value type its
/// declaration names — which is what lets an authorization policy name one
/// setting, or a wildcarded subtree of settings, as a resource. `payload` is
/// deliberately unconstrained here so that a derived type may narrow it to
/// **any** shape, scalar or object. The Schema Default is **not** here and not
/// in any derived type: it lives in the declaration's `default_value` alone, so
/// registration never gives it a second home.
#[must_use]
pub fn setting_type_base_schema() -> serde_json::Value {
    serde_json::json!({
        "$id": format!("gts://{SETTING_TYPE_BASE}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "description": "Abstract base of every setting type. A concrete setting is a type derived from this base whose payload is narrowed to the value type its declaration names; the Schema Default lives on the declaration, never in the type.",
        "type": "object",
        "properties": { "payload": {} },
        "required": ["payload"],
        "x-gts-abstract": true
    })
}

toolkit_gts::inventory::submit! {
    toolkit_gts::InventoryTypeSchema {
        type_id: SETTING_TYPE_BASE,
        schema_fn: || setting_type_base_schema().to_string(),
    }
}
// @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-9

pub const DECLARATION_SCHEMA: &str = "gts.cf.core.settings.declaration.v1~";

/// A stored setting value at some scope — what a setting currently *holds*.
pub const VALUE_SCHEMA: &str = "gts.cf.core.settings.value.v1~";

/// A settings category.
pub const CATEGORY_SCHEMA: &str = "gts.cf.core.settings.category.v1~";

/// Typed view of the wire `resource_type` strings above.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resource {
    /// [`DECLARATION_SCHEMA`]
    Declaration,
    /// [`VALUE_SCHEMA`]
    Value,
    /// [`CATEGORY_SCHEMA`]
    Category,
    /// A resource type this SDK does not model, preserved verbatim.
    Unknown(String),
}

impl Resource {
    /// Read the discriminator from a wire `resource_type` string.
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s {
            DECLARATION_SCHEMA => Self::Declaration,
            VALUE_SCHEMA => Self::Value,
            CATEGORY_SCHEMA => Self::Category,
            // Preserved rather than discarded: a consumer can still report an
            // unmodelled resource, and a later version can model it without a
            // migration.
            other => Self::Unknown(other.to_owned()),
        }
    }

    /// Render the discriminator back to its wire string.
    #[must_use]
    pub fn as_wire(&self) -> &str {
        match self {
            Self::Declaration => DECLARATION_SCHEMA,
            Self::Value => VALUE_SCHEMA,
            Self::Category => CATEGORY_SCHEMA,
            Self::Unknown(raw) => raw,
        }
    }
}

#[cfg(test)]
#[path = "gts_tests.rs"]
mod gts_tests;
