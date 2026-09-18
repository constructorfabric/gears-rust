// Created: 2026-09-17 by Constructor Tech
//! Wire shape of the search surface.

use serde_json::Value;
use settings_service_sdk::SettingKey;
use uuid::Uuid;

use crate::api::rest::setting_dto::mask;
use crate::domain::resolution::scope_path;
use crate::domain::search::MatchedField;
use crate::domain::search::service::Hit;

/// The category a hit is filed under. Categories are flat, so this is the
/// whole breadcrumb.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct SearchCategoryDto {
    /// Category identity.
    pub id: Uuid,
    /// The category slug, as carried in the setting key.
    pub key: String,
    /// Display name.
    pub name: String,
}

/// One search result. A declaration-level hit carries no scope; an override
/// hit names the scope where the matched value is set.
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct SearchHitDto {
    /// The setting key.
    pub key: String,
    /// Identity of the declaration, for a follow-up read.
    pub declaration_id: Uuid,
    /// The setting's leaf name.
    pub leaf_slug: String,
    /// The declaration's description, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The category the setting is filed under.
    pub category: SearchCategoryDto,
    /// Which field matched: `key`, `description`, `category_name`,
    /// `default_value` or `value`.
    pub matched_field: String,
    /// `standard` or `advanced`, from the declaration. A tag for grouping,
    /// never a filter — no hit is withheld by it.
    pub mode: String,
    /// The scope path where the matched override is set. Absent on a
    /// declaration-level hit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// The tenant owning that scope. Absent on a declaration-level hit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<Uuid>,
    /// The matched stored value — the override for a `value` hit, the Schema
    /// Default for a `default_value` hit — masked by classification exactly
    /// as a read would mask it. Absent for a hit on the key, description or
    /// category name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

/// Render a hit. `root` turns a tenant into its scope path; `may_read_pii` is
/// the mask decision the read surface makes.
// @cpt-dod:cpt-cf-settings-service-dod-search-discoverability-hits:p2
#[must_use]
pub fn render_hit(hit: &Hit, root: Uuid, may_read_pii: bool) -> SearchHitDto {
    let d = &hit.declaration;
    let category = hit.category.as_ref().map_or_else(
        || {
            // The row went missing between the two queries. The key still
            // carries the slug, so the breadcrumb is not lost, only its label.
            let slug = SettingKey::parse(&d.key)
                .map(|k| k.category_slug().to_owned())
                .unwrap_or_default();
            SearchCategoryDto {
                id: d.category_id,
                key: slug.clone(),
                name: slug,
            }
        },
        |c| SearchCategoryDto {
            id: c.id,
            key: c.key.as_str().to_owned(),
            name: c.name.clone(),
        },
    );
    let (scope, tenant_id, value) = match (hit.matched, &hit.row) {
        (MatchedField::Value, Some(row)) => (
            Some(scope_path(row.tenant_id, root)),
            Some(row.tenant_id),
            row.value
                .as_ref()
                .map(|v| mask(v, &row.data_classification, may_read_pii).0),
        ),
        (MatchedField::DefaultValue, _) => (
            None,
            None,
            Some(mask(&d.default_value, &d.data_classification, may_read_pii).0),
        ),
        _ => (None, None, None),
    };
    SearchHitDto {
        key: d.key.clone(),
        declaration_id: d.id,
        leaf_slug: d.leaf_slug.clone(),
        description: d.description.clone(),
        category,
        matched_field: hit.matched.as_str().to_owned(),
        mode: d.mode.clone(),
        scope,
        tenant_id,
        value,
    }
}

#[cfg(test)]
#[path = "search_dto_tests.rs"]
mod search_dto_tests;
