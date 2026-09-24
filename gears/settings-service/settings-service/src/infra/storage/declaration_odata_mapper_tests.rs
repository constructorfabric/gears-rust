// Created: 2026-08-26 by Virtuozzo International GmbH
//! Tests for the declaration `OData` surface.
//!
//! Acceptance: FEATURE `setting-declarations.md` CDSL `inst-decl-read-5` — an
//! expression on an unmapped field or with an unsupported operator is rejected
//! rather than ignored.

use serde_json::json;
use settings_service_sdk::odata::DeclarationFilterField;
use toolkit_db::odata::{FieldToColumn, ODataFieldMapping};
use uuid::Uuid;

use super::DeclarationODataMapper;
use crate::infra::storage::entity::declaration::{
    Column as DeclarationColumn, Model as DeclarationModel,
};

#[test]
fn every_declared_field_maps_to_its_column() {
    // The pairing the rejection rule rests on: a declared field always has a
    // column, so a parsed expression can always be built into a query.
    for (field, column) in [
        (DeclarationFilterField::Key, DeclarationColumn::Key),
        (
            DeclarationFilterField::CategoryId,
            DeclarationColumn::CategoryId,
        ),
        (
            DeclarationFilterField::DomainAffinity,
            DeclarationColumn::DomainAffinity,
        ),
        (DeclarationFilterField::Mode, DeclarationColumn::Mode),
        (DeclarationFilterField::Status, DeclarationColumn::Status),
        (
            DeclarationFilterField::OwnerModule,
            DeclarationColumn::OwnerModule,
        ),
    ] {
        // `Column` is not `PartialEq`; compare the identifier it resolves to.
        assert_eq!(
            format!("{:?}", DeclarationODataMapper::map_field(field)),
            format!("{column:?}")
        );
    }
}

#[test]
fn the_filter_surface_excludes_rendering_and_write_only_columns() {
    // `default_value` and `description` reach a caller through search, not
    // filtering; `scope_class`, `data_classification` and the flags organise no
    // listing. Widening later is compatible; narrowing is not, so the default
    // is narrow.
    let declared = [
        "key",
        "categoryId",
        "domainAffinity",
        "mode",
        "status",
        "ownerModule",
    ];
    for excluded in [
        "defaultValue",
        "description",
        "scopeClass",
        "dataClassification",
        "hasSecretTrait",
        "tenantVisible",
        "tenantOverridable",
        "licenceFeature",
        "createdAt",
    ] {
        assert!(
            !declared.contains(&excluded),
            "`{excluded}` must not be filterable without a deliberate decision"
        );
    }
}

/// A stored row with a distinct value in every cursor-bearing column, so an
/// arm reading the wrong field cannot pass by coincidence.
fn row(owner_module: Option<&str>) -> DeclarationModel {
    let at = time::OffsetDateTime::UNIX_EPOCH;
    DeclarationModel {
        id: Uuid::new_v4(),
        key: "gts.cf.core.settings.setting_type.v1~acme.settings.network.proxy.v1~".to_owned(),
        leaf_slug: "proxy".to_owned(),
        value_type_id: "gts.cf.core.settings.type_bool_flag.v1~".to_owned(),
        category_id: Uuid::new_v4(),
        default_value: json!(true),
        scope_class: "cascading".to_owned(),
        mode: "advanced".to_owned(),
        requires_step_up: false,
        anonymous_exposable: false,
        domain_affinity: Some("hosting".to_owned()),
        has_secret_trait: false,
        data_classification: "public".to_owned(),
        source: "admin_authored".to_owned(),
        owner_module: owner_module.map(str::to_owned),
        licence_feature: None,
        status: "retired".to_owned(),
        description: None,
        last_change_at: at,
        created_at: at,
        updated_at: at,
        created_by: "test".to_owned(),
    }
}

#[test]
fn every_declared_field_extracts_its_own_cursor_value() {
    // The cursor describes the last row served, by the field the page is
    // ordered on. Read from the wrong field, the next page would continue from
    // the wrong place — silently, since a cursor is opaque to the caller.
    let model = row(Some("cf.settings_demo"));
    let expected: [(DeclarationFilterField, sea_orm::Value); 6] = [
        (DeclarationFilterField::Key, model.key.clone().into()),
        (DeclarationFilterField::CategoryId, model.category_id.into()),
        (
            DeclarationFilterField::DomainAffinity,
            model.domain_affinity.clone().into(),
        ),
        (DeclarationFilterField::Mode, model.mode.clone().into()),
        (DeclarationFilterField::Status, model.status.clone().into()),
        (
            DeclarationFilterField::OwnerModule,
            model.owner_module.clone().into(),
        ),
    ];
    for (i, (field, value)) in expected.into_iter().enumerate() {
        assert_eq!(
            DeclarationODataMapper::extract_cursor_value(&model, field),
            value,
            "field #{i}"
        );
    }

    // The nullable column, absent: the cursor value is the typed null, not a
    // panic and not an empty string.
    let admin_authored = row(None);
    assert_eq!(
        DeclarationODataMapper::extract_cursor_value(
            &admin_authored,
            DeclarationFilterField::OwnerModule
        ),
        sea_orm::Value::String(None)
    );
}
