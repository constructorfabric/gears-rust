//! GTS resource types this gear attributes its errors to, and the GTS schemas
//! the gear publishes.
//!
//! The resource types below also leave this crate as authorization-label stubs
//! (submitted next to the setting-type base, published with the gear's
//! manifest), so a role-definition that names one of them resolves. A stub
//! registers the identifier and constrains nothing: it does not describe the
//! category, the declaration or the stored value. They are
//! the values that reach `NotFound.ctx.resource_type` and its
//! siblings. They are the discriminator the [`crate::SettingsError`] projection
//! uses to tell two otherwise identical `NotFound` outcomes apart: a setting
//! that was never declared, and a secret-backed setting whose credential is not
//! configured at any scope. A consumer that conflates the two hands a
//! placeholder to its backend believing it to be a credential.

/// The abstract base every setting key derives from, as a wire string.
///
/// A setting key is this followed by the setting's own derived type (ADR-002).
/// The schema registered under it is [`setting_type_base_schema`] below.
pub const SETTING_TYPE_BASE: &str = toolkit_gts::gts_id!("cf.core.settings.setting_type.v1~");

// @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-9
/// The schema of the abstract base every concrete setting type derives from.
///
/// Published with the gear's manifest: the submission below, to this crate's
/// collector (`gts_declarations()`), happens at link time, and the runtime
/// publishes the collector through the gear's `gts_publication(...)` and waits
/// for its admission before any application `init` — so the base exists before
/// any declaration path composes a derived type from it. No call is made from
/// this gear's init, and there is nothing to retry. The process-wide inventory
/// an embedded types registry drains at its own init gets the same entry.
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

submit_type_schema!(SETTING_TYPE_BASE, || setting_type_base_schema().to_string());
// @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-9

/// A setting declaration — the record of what a setting *is*.
pub const DECLARATION_SCHEMA: &str = toolkit_gts::gts_id!("cf.core.settings.declaration.v1~");

/// A stored setting value at some scope — what a setting currently *holds*.
pub const VALUE_SCHEMA: &str = toolkit_gts::gts_id!("cf.core.settings.value.v1~");

/// A settings category.
pub const CATEGORY_SCHEMA: &str = toolkit_gts::gts_id!("cf.core.settings.category.v1~");

// # Why the resource types are published as stubs
//
// The permission catalogue names these types as `resource_type`, and the
// platform role-definition validator resolves a rule's target type through the
// types registry, so each must be registered. What is registered is an
// authorization label, not the shape of the resource: the registry lets a
// published type only be widened in place and gives publishers no ordering, so
// a body that tracked the gear's representations would differ between two
// builds of the gear, and the older build (a rollback, a restart in the middle
// of a rollout) would be refused at publication and would not start. A stub is
// the same document in every build, so its publication is always a repeat.

/// The body published under a resource type: an authorization label.
///
/// Draft-07, `type: object` and nothing else — no `properties`, no `required`,
/// no `additionalProperties`. The key set and every value are fixed by
/// `type_id` and `title`, so every build of the gear publishes the same
/// document.
fn authz_label_schema(type_id: &str, title: &str) -> serde_json::Value {
    serde_json::json!({
        "$id": format!("gts://{type_id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": title,
        "description": "Authorization label: this identifier is what a role definition names as the target type of a rule. The body deliberately constrains nothing and does not describe the resource's representation.",
        "type": "object"
    })
}

/// The stub registered under [`CATEGORY_SCHEMA`]: an authorization label, not
/// a description of a category (see [`authz_label_schema`]).
fn category_schema() -> serde_json::Value {
    authz_label_schema(CATEGORY_SCHEMA, "Settings category")
}

/// The stub registered under [`DECLARATION_SCHEMA`]: an authorization label,
/// not a description of a declaration (see [`authz_label_schema`]).
fn declaration_schema() -> serde_json::Value {
    authz_label_schema(DECLARATION_SCHEMA, "Setting declaration")
}

/// The stub registered under [`VALUE_SCHEMA`]: an authorization label, not a
/// description of a stored value (see [`authz_label_schema`]).
fn value_schema() -> serde_json::Value {
    authz_label_schema(VALUE_SCHEMA, "Setting value")
}

submit_type_schema!(DECLARATION_SCHEMA, || declaration_schema().to_string());

submit_type_schema!(VALUE_SCHEMA, || value_schema().to_string());

submit_type_schema!(CATEGORY_SCHEMA, || category_schema().to_string());

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
