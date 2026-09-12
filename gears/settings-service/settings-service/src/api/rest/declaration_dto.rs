// Created: 2026-08-26 by Constructor Tech
//! Wire shapes for the declaration read endpoints.

use serde_json::Value;
use uuid::Uuid;

use crate::domain::declaration::service::RenderedDeclaration;

/// A declaration as returned to a caller.
///
/// Carries `key`, `value_type_id` and the resolved trait set, which is what the
/// read surface is specified to render. The value type travels beside the key
/// because the key no longer carries it: a setting key is a GTS *type* id
/// (ADR-002), and which value type its default and overrides validate against
/// is a separate fact of the declaration, not a half of its name.
// `Eq` is absent because `traits` is a `serde_json::Value`, which is only
// `PartialEq` -- JSON numbers have no total equality.
// The flags mirror the declaration's: separate facts an administrator reads
// one by one.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct DeclarationDto {
    /// Server-assigned identity, stable across a re-key.
    pub id: Uuid,
    /// The full setting key.
    pub key: String,
    /// The setting's own name slug, unique within its category.
    pub leaf_slug: String,
    /// GTS id of the value type the default and every override validate against.
    pub value_type_id: String,
    /// Owning category.
    pub category_id: Uuid,
    /// `global`, `cascading`, or `local`.
    pub scope_class: String,
    /// `standard` or `advanced`.
    pub mode: String,
    /// `active` or `retired`.
    pub status: String,
    /// The administrative domain, when the declaration is bound to one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain_affinity: Option<String>,
    /// The licence feature that will gate this declaration once the License
    /// Resolver exists. Reported, not enforced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub licence_feature: Option<String>,
    /// The contributing module, for module-contributed declarations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_module: Option<String>,
    /// Optional long-form description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The value type's effective traits, merged across its inheritance chain.
    ///
    /// An empty object when the registry could not answer. Always present so a
    /// client renders the same shape either way rather than branching on
    /// absence.
    /// The Schema Default every resolution chain ends in.
    pub default_value: Value,
    /// `public`, `pii` or `secret`; `secret` is derived from the value type.
    pub data_classification: String,
    /// Whether the value type carries the `secret` trait.
    pub has_secret_trait: bool,
    /// Whether a value write needs a fresh re-authentication.
    pub requires_step_up: bool,
    /// Whether the anonymous surface may show the value.
    pub anonymous_exposable: bool,
    /// `admin_authored` or `module_contributed`.
    pub source: String,
    /// When the definition last changed, RFC 3339.
    pub last_change_at: String,
    /// The declaration's state tag, also sent as the `ETag` header.
    pub etag: String,
    pub traits: Value,
}

/// The declaration's state tag: its normalized UTC `updated_at`, as every
/// other tag in this service.
#[must_use]
pub fn declaration_etag(declaration: &crate::domain::declaration::Declaration) -> String {
    declaration.updated_at.unix_timestamp_nanos().to_string()
}

impl From<RenderedDeclaration> for DeclarationDto {
    fn from(rendered: RenderedDeclaration) -> Self {
        let d = rendered.declaration;
        let etag = declaration_etag(&d);
        let last_change_at = d
            .last_change_at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| d.last_change_at.to_string());
        Self {
            id: d.id,
            key: d.key,
            leaf_slug: d.leaf_slug,
            value_type_id: d.value_type_id,
            category_id: d.category_id,
            scope_class: d.scope_class,
            mode: d.mode,
            status: d.status,
            domain_affinity: d.domain_affinity,
            licence_feature: d.licence_feature,
            owner_module: d.owner_module,
            description: d.description,
            default_value: d.default_value,
            data_classification: d.data_classification,
            has_secret_trait: d.has_secret_trait,
            requires_step_up: d.requires_step_up,
            anonymous_exposable: d.anonymous_exposable,
            source: d.source,
            last_change_at,
            etag,
            traits: rendered.traits,
        }
    }
}

/// What an administrator supplies to declare a setting.
///
/// The key is not among the fields: the service composes it from the vendor,
/// the category's slug and the leaf name, so a caller cannot mint a key that
/// disagrees with where the setting is filed. `default_value` is mandatory --
/// it is what makes resolution total -- and a setting with no meaningful
/// default sends JSON `null` on a type that admits it; omitting the field is a
/// different thing and is refused.
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct CreateDeclarationRequest {
    /// The curated value type the default and every override validate against.
    pub value_type_id: String,
    /// The vendor segment of the composed key.
    pub vendor: String,
    /// The leaf name: the last segment of the key, unique in the category.
    pub name: String,
    /// The category the setting is filed under; its slug rides in the key.
    pub category_id: Uuid,
    /// The Schema Default. Mandatory, and an empty placeholder for a secret.
    pub default_value: Value,
    /// `global`, `cascading` or `local`.
    pub scope_class: String,
    /// Optional long-form description.
    #[serde(default)]
    pub description: Option<String>,
    /// `standard` or `advanced`; `standard` when absent.
    #[serde(default)]
    pub mode: Option<String>,
    /// Whether a write needs a fresh re-authentication; the protective `true`
    /// when absent.
    #[serde(default)]
    pub requires_step_up: Option<bool>,
    /// Whether the effective value may be served unauthenticated; `false` when
    /// absent, and refused on a `pii` or `secret` setting.
    #[serde(default)]
    pub anonymous_exposable: Option<bool>,
    /// Optional administrative domain.
    #[serde(default)]
    pub domain_affinity: Option<String>,
    /// Optional licence feature gating the setting in R2.
    #[serde(default)]
    pub licence_feature: Option<String>,
    /// `public` or `pii`; `secret` is derived from the value type's trait and
    /// is refused here.
    #[serde(default)]
    pub data_classification: Option<String>,
}

impl From<CreateDeclarationRequest> for crate::domain::declaration::CreateDeclaration {
    fn from(body: CreateDeclarationRequest) -> Self {
        Self {
            value_type_id: body.value_type_id,
            vendor: body.vendor,
            name: body.name,
            category_id: body.category_id,
            default_value: body.default_value,
            scope_class: body.scope_class,
            description: body.description,
            mode: body.mode,
            requires_step_up: body.requires_step_up,
            anonymous_exposable: body.anonymous_exposable,
            domain_affinity: body.domain_affinity,
            licence_feature: body.licence_feature,
            data_classification: body.data_classification,
        }
    }
}

/// What an administrator may supply to edit a declaration's metadata.
///
/// This shape exists for the schema the operation publishes; the handler reads
/// the raw object, because what matters is which fields are *present*: an
/// omitted field is untouched, an explicit `null` clears a nullable one, and a
/// field this shape does not carry is refused by name rather than ignored.
/// A typed struct cannot express the first distinction, so it documents the
/// surface while the handler enforces it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[toolkit_macros::api_dto(request)]
pub struct UpdateDeclarationRequest {
    /// Long-form description; `null` clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `standard` or `advanced`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Administrative domain; `null` clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_affinity: Option<String>,
    /// Licence feature; `null` clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub licence_feature: Option<String>,
    /// Setting it `true` is immediate; clearing it requires step-up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_step_up: Option<bool>,
    /// Setting it `false` is immediate; enabling it requires step-up and is
    /// refused on a `pii` or `secret` setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anonymous_exposable: Option<bool>,
    /// `public` or `pii`. Tightening is immediate, loosening requires step-up,
    /// and `secret` is derived from the value type and refused here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_classification: Option<String>,
}

/// The response to a create: the declaration, and whether a retired one was
/// revived rather than a new row inserted.
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct CreatedDeclarationDto {
    /// The declaration now live at the key.
    #[serde(flatten)]
    pub declaration: DeclarationDto,
    /// `true` when the key held a retired declaration that this call revived,
    /// with its retained values re-entering resolution.
    pub reactivated: bool,
}

#[cfg(test)]
#[path = "declaration_dto_tests.rs"]
mod declaration_dto_tests;
