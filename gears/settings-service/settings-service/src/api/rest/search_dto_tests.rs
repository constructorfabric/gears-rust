// Created: 2026-09-17 by Virtuozzo International GmbH
//! What a hit looks like on the wire: a scope only where a value is set, a
//! value only where one matched, and the mask a read would apply.

use std::sync::Arc;

use serde_json::json;
use uuid::Uuid;

use super::render_hit;
use crate::domain::category::Category;
use crate::domain::declaration::Declaration;
use crate::domain::resolution::MASK_TOKEN;
use crate::domain::search::MatchedField;
use crate::domain::search::service::Hit;
use crate::domain::value::StoredValue;

fn at(seconds: i64) -> time::OffsetDateTime {
    time::OffsetDateTime::from_unix_timestamp(seconds).expect("in range")
}

const KEY: &str = "gts.cf.core.settings.setting_type.v1~acme.settings.logging.retention_days.v1~";

fn declaration(classification: &str) -> Arc<Declaration> {
    Arc::new(Declaration {
        id: Uuid::from_u128(2),
        key: KEY.to_owned(),
        leaf_slug: "retention_days".to_owned(),
        value_type_id: "gts.cf.core.settings.type_integer.v1~".to_owned(),
        category_id: Uuid::from_u128(3),
        scope_class: "cascading".to_owned(),
        mode: "advanced".to_owned(),
        status: "active".to_owned(),
        domain_affinity: None,
        licence_feature: None,
        owner_module: None,
        description: Some("How long audit records are retained.".to_owned()),
        default_value: json!(30),
        has_secret_trait: false,
        data_classification: classification.to_owned(),
        requires_step_up: false,
        anonymous_exposable: false,
        source: "admin_authored".to_owned(),
        last_change_at: at(0),
        updated_at: at(0),
    })
}

fn category() -> Arc<Category> {
    Arc::new(Category {
        id: Uuid::from_u128(3),
        key: crate::domain::category::key::CategoryKey::parse("logging").expect("slug"),
        name: "Logging".to_owned(),
        description: None,
        domain_affinity: None,
        sort_order: 0,
        icon: None,
        etag: crate::domain::precondition::ETag::new("1"),
    })
}

fn row(tenant: Uuid, value: serde_json::Value, classification: &str) -> StoredValue {
    StoredValue {
        id: Uuid::new_v4(),
        declaration_id: Uuid::from_u128(2),
        tenant_id: tenant,
        value: Some(value),
        secret_ref: None,
        data_classification: classification.to_owned(),
        needs_review: false,
        needs_review_detail: None,
        last_change_at: at(1),
        updated_at: at(1),
        set_by: "admin".to_owned(),
    }
}

#[test]
fn a_declaration_level_hit_carries_the_breadcrumb_and_no_scope_or_value() {
    let hit = Hit {
        declaration: declaration("public"),
        category: Some(category()),
        matched: MatchedField::Description,
        row: None,
    };
    let dto = render_hit(&hit, Uuid::from_u128(1), false);
    assert_eq!(dto.key, KEY);
    assert_eq!(dto.declaration_id, Uuid::from_u128(2));
    assert_eq!(dto.leaf_slug, "retention_days");
    assert_eq!(dto.matched_field, "description");
    assert_eq!(dto.mode, "advanced", "a tag on every hit, never a filter");
    assert_eq!(
        (
            dto.category.id,
            dto.category.key.as_str(),
            dto.category.name.as_str()
        ),
        (Uuid::from_u128(3), "logging", "Logging")
    );
    let wire = serde_json::to_value(&dto).expect("serializes");
    for absent in ["scope", "tenant_id", "value"] {
        assert!(
            wire.get(absent).is_none(),
            "{absent} is skipped, not null: {wire}"
        );
    }
    assert_eq!((dto.scope, dto.tenant_id, dto.value), (None, None, None));
}

#[test]
fn an_override_hit_names_the_scope_where_the_value_is_set() {
    let root = Uuid::from_u128(1);
    let tenant = Uuid::from_u128(9);
    let hit = Hit {
        declaration: declaration("public"),
        category: Some(category()),
        matched: MatchedField::Value,
        row: Some(row(tenant, json!(90), "public")),
    };
    let dto = render_hit(&hit, root, false);
    assert_eq!(dto.matched_field, "value");
    assert_eq!(
        dto.scope.as_deref(),
        Some(format!("/tenants/{tenant}").as_str())
    );
    assert_eq!(dto.tenant_id, Some(tenant));
    assert_eq!(dto.value, Some(json!(90)));

    let at_root = Hit {
        row: Some(row(root, json!(7), "public")),
        ..hit
    };
    assert_eq!(
        render_hit(&at_root, root, false).scope.as_deref(),
        Some("/")
    );
}

#[test]
fn a_default_hit_carries_the_default_and_no_scope() {
    let hit = Hit {
        declaration: declaration("public"),
        category: Some(category()),
        matched: MatchedField::DefaultValue,
        row: None,
    };
    let dto = render_hit(&hit, Uuid::from_u128(1), false);
    assert_eq!(dto.matched_field, "default_value");
    assert_eq!(dto.value, Some(json!(30)));
    assert_eq!((dto.scope, dto.tenant_id), (None, None));
}

#[test]
fn a_value_is_masked_as_a_read_would_mask_it() {
    // A pii value reaches a hit only for an entitled caller, so the mask is
    // normally a no-op here; it stays the same decision as the read's so the
    // two surfaces cannot drift apart.
    let hit = Hit {
        declaration: declaration("pii"),
        category: Some(category()),
        matched: MatchedField::DefaultValue,
        row: None,
    };
    assert_eq!(
        render_hit(&hit, Uuid::from_u128(1), true).value,
        Some(json!(30))
    );
    assert_eq!(
        render_hit(&hit, Uuid::from_u128(1), false).value,
        Some(json!(MASK_TOKEN))
    );
}

#[test]
fn a_missing_category_row_falls_back_to_the_slug_the_key_carries() {
    let hit = Hit {
        declaration: declaration("public"),
        category: None,
        matched: MatchedField::Key,
        row: None,
    };
    let dto = render_hit(&hit, Uuid::from_u128(1), false);
    assert_eq!(dto.category.id, Uuid::from_u128(3));
    assert_eq!(dto.category.key, "logging");
    assert_eq!(dto.category.name, "logging");
}
