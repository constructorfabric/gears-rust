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
    pub traits: Value,
}

impl From<RenderedDeclaration> for DeclarationDto {
    fn from(rendered: RenderedDeclaration) -> Self {
        let d = rendered.declaration;
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
            traits: rendered.traits,
        }
    }
}

#[cfg(test)]
#[path = "declaration_dto_tests.rs"]
mod declaration_dto_tests;
