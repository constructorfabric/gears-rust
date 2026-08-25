// Created: 2026-08-13 by Constructor Tech
//! Wire shapes for the category endpoints.
//!
//! Separate from the domain [`Category`](crate::domain::category::Category) on
//! purpose. The domain type carries an [`ETag`](crate::api::precondition::ETag)
//! that travels as a **header**, not a body field, and a client that could send
//! `id` or `etag` in a request body would be claiming an identity or forging a
//! precondition. What a caller may set and what the service assigns are
//! different sets, so they are different types.

use uuid::Uuid;

use crate::domain::category::{Category, CategoryDraft, CategoryKey, CategoryPatch, bounds};
use crate::domain::error::DomainError;

/// A category as returned to a caller.
///
/// Field naming is `snake_case`, applied by `#[toolkit_macros::api_dto]` rather
/// than chosen here: every gear's DTOs serialize the same way, so a client does
/// not have to know which service produced a payload.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct CategoryDto {
    /// Server-assigned identity, stable across a rename.
    pub id: Uuid,
    /// The stable slug that becomes a setting key's category segment.
    pub key: String,
    /// Display name.
    pub name: String,
    /// Optional long-form description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional domain this category belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain_affinity: Option<String>,
    /// Ordering weight; lower sorts first.
    pub sort_order: i32,
    /// Optional icon reference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

impl From<Category> for CategoryDto {
    fn from(category: Category) -> Self {
        // `etag` is deliberately not carried: it is a response *header*, and
        // duplicating it in the body would give a client two sources for one
        // precondition, one of which it might send back stale.
        Self {
            id: category.id,
            key: category.key.as_str().to_owned(),
            name: category.name,
            description: category.description,
            domain_affinity: category.domain_affinity,
            sort_order: category.sort_order,
            icon: category.icon,
        }
    }
}

/// What a caller supplies to create a category.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct CreateCategoryRequest {
    /// The stable slug. Validated before anything else touches it.
    pub key: String,
    /// Display name.
    pub name: String,
    /// Optional long-form description.
    #[serde(default)]
    pub description: Option<String>,
    /// Optional domain affinity.
    #[serde(default)]
    pub domain_affinity: Option<String>,
    /// Ordering weight; defaults to 0 as DESIGN.md §4.7 specifies.
    #[serde(default)]
    pub sort_order: i32,
    /// Optional icon reference.
    #[serde(default)]
    pub icon: Option<String>,
}

/// What a caller supplies to replace a category's mutable fields.
///
/// A full replacement, so an omitted optional field clears it rather than
/// leaving it untouched. A partial update would need a distinct shape that can
/// tell "absent" from "set to null", and the design does not ask for one.
///
/// `key` is accepted by the parser and then refused, rather than removed from
/// the shape. `deny_unknown_fields` would reject a supplied `key` as an
/// *unknown* field, which is untrue -- the field exists, it is returned in
/// every response, and it simply may not be changed. A caller deserves to be
/// told that, not that we have never heard of it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct UpdateCategoryRequest {
    /// The stable slug. Accepted only to be refused: see the type docs.
    #[serde(default)]
    pub key: Option<String>,
    /// Display name.
    pub name: String,
    /// Optional long-form description.
    #[serde(default)]
    pub description: Option<String>,
    /// Optional domain affinity.
    #[serde(default)]
    pub domain_affinity: Option<String>,
    /// Ordering weight.
    #[serde(default)]
    pub sort_order: i32,
    /// Optional icon reference.
    #[serde(default)]
    pub icon: Option<String>,
}

impl CreateCategoryRequest {
    /// Validate into a draft.
    ///
    /// # Errors
    /// [`DomainError::Validation`] when the key breaks its format rules.
    pub fn into_draft(self) -> Result<CategoryDraft, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-6
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-7
        // Checked here rather than left to the column: the driver reports an
        // over-long value as an opaque write failure, which `map_write_error`
        // can only classify as internal.
        bounds::validate(&self.name, self.description.as_deref())?;
        // @cpt-end:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-7
        // @cpt-end:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-6
        Ok(CategoryDraft {
            // @cpt-begin:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-4
            // @cpt-begin:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-5
            // Validated on the way in, before the draft can reach a repository.
            // The rejection carries `key` as its field, so the problem document
            // points at what the caller sent rather than at the body as a whole.
            key: CategoryKey::parse(&self.key)?,
            // @cpt-end:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-5
            // @cpt-end:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-4
            name: self.name,
            description: self.description,
            domain_affinity: self.domain_affinity,
            sort_order: self.sort_order,
            icon: self.icon,
        })
    }
}

impl UpdateCategoryRequest {
    /// Validate into a patch.
    ///
    /// # Errors
    /// [`DomainError::Validation`] when the body carries a `key`.
    pub fn into_patch(self) -> Result<CategoryPatch, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-4
        // Refused whether or not it matches the current key: the rule is that an
        // update does not carry one, and accepting an echo would make the wire
        // contract depend on a value the caller cannot change anyway.
        if self.key.is_some() {
            return Err(DomainError::Validation {
                field: "key".to_owned(),
                code: crate::field::CATEGORY_KEY_IMMUTABLE,
                message: "a category key is immutable: settings are keyed through it, \
                          and changing it here would re-key every setting in the category"
                    .to_owned(),
            });
        }
        // @cpt-end:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-4
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-10
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-11
        // The same bounds as create: an update is a full replacement, so
        // every field it carries faces the rule its column enforces.
        bounds::validate(&self.name, self.description.as_deref())?;
        // @cpt-end:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-11
        // @cpt-end:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-10
        Ok(CategoryPatch {
            name: self.name,
            description: self.description,
            domain_affinity: self.domain_affinity,
            sort_order: self.sort_order,
            icon: self.icon,
        })
    }
}

#[cfg(test)]
#[path = "dto_tests.rs"]
mod dto_tests;
