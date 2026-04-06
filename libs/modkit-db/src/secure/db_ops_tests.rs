use super::*;
use sea_orm::entity::prelude::*;

// Test entity with tenant_col for SecureOnConflict tests
mod test_entity {
    use super::*;
    use modkit_security::pep_properties;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "test_table")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub name: String,
        pub value: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}

    impl ScopableEntity for Entity {
        fn tenant_col() -> Option<Column> {
            Some(Column::TenantId)
        }
        fn resource_col() -> Option<Column> {
            Some(Column::Id)
        }
        fn owner_col() -> Option<Column> {
            None
        }
        fn type_col() -> Option<Column> {
            None
        }
        fn resolve_property(property: &str) -> Option<Column> {
            match property {
                pep_properties::OWNER_TENANT_ID => Self::tenant_col(),
                pep_properties::RESOURCE_ID => Self::resource_col(),
                _ => None,
            }
        }
    }
}

// Test entity without tenant_col (global entity)
mod global_entity {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "global_table")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: Uuid,
        pub config_key: String,
        pub config_value: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}

    impl ScopableEntity for Entity {
        fn tenant_col() -> Option<Column> {
            None // Global entity - no tenant column
        }
        fn resource_col() -> Option<Column> {
            Some(Column::Id)
        }
        fn owner_col() -> Option<Column> {
            None
        }
        fn type_col() -> Option<Column> {
            None
        }
        fn resolve_property(property: &str) -> Option<Column> {
            match property {
                "id" => Self::resource_col(),
                _ => None,
            }
        }
    }
}

#[test]
fn test_validate_tenant_in_scope() {
    let tenant_id = uuid::Uuid::new_v4();
    let scope = crate::secure::AccessScope::for_tenants(vec![tenant_id]);

    assert!(validate_tenant_in_scope(tenant_id, &scope).is_ok());

    let other_id = uuid::Uuid::new_v4();
    assert!(validate_tenant_in_scope(other_id, &scope).is_err());
}

// Note: Full integration tests with database require actual SeaORM entities
// These tests verify the typestate pattern compiles correctly

#[test]
fn test_typestate_compile_check() {
    // This test verifies the typestate markers compile
    let unscoped: PhantomData<Unscoped> = PhantomData;
    let scoped: PhantomData<Scoped> = PhantomData;
    // Use the variables to avoid unused warnings
    let _ = (unscoped, scoped);
}

#[test]
fn test_tenant_not_in_scope_returns_error() {
    // Verify that validate_tenant_in_scope properly rejects tenant IDs not in scope
    let allowed_tenant = uuid::Uuid::new_v4();
    let disallowed_tenant = uuid::Uuid::new_v4();
    let scope = crate::secure::AccessScope::for_tenants(vec![allowed_tenant]);

    // Allowed tenant should succeed
    assert!(validate_tenant_in_scope(allowed_tenant, &scope).is_ok());

    // Disallowed tenant should fail with TenantNotInScope error
    let result = validate_tenant_in_scope(disallowed_tenant, &scope);
    assert!(result.is_err());
    match result {
        Err(ScopeError::TenantNotInScope { tenant_id }) => {
            assert_eq!(tenant_id, disallowed_tenant);
        }
        _ => panic!("Expected TenantNotInScope error"),
    }
}

#[test]
fn test_empty_scope_denied_for_tenant_scoped() {
    // Verify that an empty scope (no tenants) is rejected for tenant-scoped inserts
    let tenant_id = uuid::Uuid::new_v4();
    let empty_scope = crate::secure::AccessScope::default();

    let result = validate_tenant_in_scope(tenant_id, &empty_scope);
    assert!(result.is_err());
    match result {
        Err(ScopeError::Denied(_)) => {}
        _ => panic!("Expected Denied error for empty scope"),
    }
}

// SecureOnConflict tests

#[test]
fn test_secure_on_conflict_update_columns_allows_non_tenant_columns() {
    use test_entity::{Column, Entity};

    // update_columns with non-tenant columns should succeed
    let result = SecureOnConflict::<Entity>::columns([Column::Id])
        .update_columns([Column::Name, Column::Value]);

    assert!(result.is_ok());
}

#[test]
fn test_secure_on_conflict_update_columns_rejects_tenant_column() {
    use test_entity::{Column, Entity};

    // update_columns with tenant_id should fail
    let result = SecureOnConflict::<Entity>::columns([Column::Id]).update_columns([
        Column::Name,
        Column::TenantId,
        Column::Value,
    ]);

    assert!(result.is_err());
    match result {
        Err(ScopeError::Denied(msg)) => {
            assert!(msg.contains("immutable"), "Expected immutable error: {msg}");
        }
        _ => panic!("Expected Denied error for tenant_id in update_columns"),
    }
}

#[test]
fn test_secure_on_conflict_value_allows_non_tenant_columns() {
    use sea_orm::sea_query::Expr;
    use test_entity::{Column, Entity};

    // value() with non-tenant column should succeed
    let result =
        SecureOnConflict::<Entity>::columns([Column::Id]).value(Column::Name, Expr::value("test"));

    assert!(result.is_ok());
}

#[test]
fn test_secure_on_conflict_value_rejects_tenant_column() {
    use sea_orm::sea_query::Expr;
    use test_entity::{Column, Entity};

    // value() with tenant_id should fail
    let result = SecureOnConflict::<Entity>::columns([Column::Id])
        .value(Column::TenantId, Expr::value(uuid::Uuid::new_v4()));

    assert!(result.is_err());
    match result {
        Err(ScopeError::Denied(msg)) => {
            assert!(msg.contains("immutable"), "Expected immutable error: {msg}");
        }
        _ => panic!("Expected Denied error for tenant_id in value()"),
    }
}

#[test]
fn test_secure_on_conflict_chained_value_rejects_tenant_column() {
    use sea_orm::sea_query::Expr;
    use test_entity::{Column, Entity};

    // Chaining value() calls - should fail when tenant_id is added
    let result = SecureOnConflict::<Entity>::columns([Column::Id])
        .value(Column::Name, Expr::value("test"))
        .and_then(|c| c.value(Column::TenantId, Expr::value(uuid::Uuid::new_v4())));

    assert!(result.is_err());
    match result {
        Err(ScopeError::Denied(msg)) => {
            assert!(msg.contains("immutable"), "Expected immutable error: {msg}");
        }
        _ => panic!("Expected Denied error for tenant_id in chained value()"),
    }
}

#[test]
fn test_secure_on_conflict_global_entity_allows_all_columns() {
    use global_entity::{Column, Entity};

    // Global entity has no tenant_col, so all columns are allowed
    let result = SecureOnConflict::<Entity>::columns([Column::Id])
        .update_columns([Column::ConfigKey, Column::ConfigValue]);

    assert!(result.is_ok());
}

#[test]
fn test_secure_on_conflict_build_produces_on_conflict() {
    use test_entity::{Column, Entity};

    // Verify that build() produces a valid OnConflict
    let on_conflict = SecureOnConflict::<Entity>::columns([Column::Id])
        .update_columns([Column::Name, Column::Value])
        .expect("should succeed")
        .build();

    // The OnConflict should be usable (we can't easily test its internals,
    // but we can verify it doesn't panic)
    _ = format!("{on_conflict:?}");
}

// ── validate_insert_scope tests ─────────────────────────────────

// Test entity with owner_col and a custom pep_prop (city_id),
// mimicking the Address entity from the users-info example.
mod owner_entity {
    use super::*;
    use modkit_security::pep_properties;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "addresses")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub user_id: Uuid,
        pub city_id: Uuid,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}

    impl ScopableEntity for Entity {
        fn tenant_col() -> Option<Column> {
            Some(Column::TenantId)
        }
        fn resource_col() -> Option<Column> {
            Some(Column::Id)
        }
        fn owner_col() -> Option<Column> {
            Some(Column::UserId)
        }
        fn type_col() -> Option<Column> {
            None
        }
        fn resolve_property(property: &str) -> Option<Column> {
            match property {
                pep_properties::OWNER_TENANT_ID => Some(Column::TenantId),
                pep_properties::RESOURCE_ID => Some(Column::Id),
                pep_properties::OWNER_ID => Some(Column::UserId),
                "city_id" => Some(Column::CityId),
                _ => None,
            }
        }
    }
}

#[test]
fn test_validate_insert_scope_allow_all_passes() {
    use owner_entity::ActiveModel;
    use sea_orm::Set;

    let scope = crate::secure::AccessScope::allow_all();
    let am = ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(Uuid::new_v4()),
        user_id: Set(Uuid::new_v4()),
        city_id: Set(Uuid::new_v4()),
    };
    assert!(validate_insert_scope(&am, &scope).is_ok());
}

#[test]
fn test_validate_insert_scope_deny_all_rejects() {
    use owner_entity::ActiveModel;
    use sea_orm::Set;

    let scope = crate::secure::AccessScope::deny_all();
    let am = ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(Uuid::new_v4()),
        user_id: Set(Uuid::new_v4()),
        city_id: Set(Uuid::new_v4()),
    };
    assert!(validate_insert_scope(&am, &scope).is_err());
}

#[test]
fn test_validate_insert_scope_tenant_only_matches() {
    use owner_entity::ActiveModel;
    use sea_orm::Set;

    let tenant_id = Uuid::new_v4();
    let scope = crate::secure::AccessScope::for_tenant(tenant_id);
    let am = ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        user_id: Set(Uuid::new_v4()),
        city_id: Set(Uuid::new_v4()),
    };
    assert!(validate_insert_scope(&am, &scope).is_ok());
}

#[test]
fn test_validate_insert_scope_tenant_mismatch_rejects() {
    use owner_entity::ActiveModel;
    use sea_orm::Set;

    let tenant_id = Uuid::new_v4();
    let other_tenant = Uuid::new_v4();
    let scope = crate::secure::AccessScope::for_tenant(tenant_id);
    let am = ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(other_tenant),
        user_id: Set(Uuid::new_v4()),
        city_id: Set(Uuid::new_v4()),
    };
    assert!(validate_insert_scope(&am, &scope).is_err());
}

#[test]
fn test_validate_insert_scope_owner_id_matches() {
    use modkit_security::access_scope::{ScopeConstraint, ScopeFilter};
    use modkit_security::pep_properties;
    use owner_entity::ActiveModel;
    use sea_orm::Set;

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let city_id = Uuid::new_v4();

    // Scope: tenant + owner_id + city_id (all must match)
    let scope = AccessScope::from_constraints(vec![ScopeConstraint::new(vec![
        ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![tenant_id]),
        ScopeFilter::eq(pep_properties::OWNER_ID, user_id),
        ScopeFilter::eq("city_id", city_id),
    ])]);

    let am = ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        user_id: Set(user_id),
        city_id: Set(city_id),
    };
    assert!(
        validate_insert_scope(&am, &scope).is_ok(),
        "Insert should pass when all properties match"
    );
}

#[test]
fn test_validate_insert_scope_owner_id_mismatch_rejects() {
    use modkit_security::access_scope::{ScopeConstraint, ScopeFilter};
    use modkit_security::pep_properties;
    use owner_entity::ActiveModel;
    use sea_orm::Set;

    let tenant_id = Uuid::new_v4();
    let user_a = Uuid::new_v4();
    let user_b = Uuid::new_v4();
    let city_id = Uuid::new_v4();

    // Scope says owner_id must be user_a
    let scope = AccessScope::from_constraints(vec![ScopeConstraint::new(vec![
        ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![tenant_id]),
        ScopeFilter::eq(pep_properties::OWNER_ID, user_a),
        ScopeFilter::eq("city_id", city_id),
    ])]);

    // But ActiveModel has user_id = user_b
    let am = ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        user_id: Set(user_b),
        city_id: Set(city_id),
    };
    assert!(
        validate_insert_scope(&am, &scope).is_err(),
        "Insert must be rejected when owner_id doesn't match"
    );
}

#[test]
fn test_validate_insert_scope_city_id_mismatch_rejects() {
    use modkit_security::access_scope::{ScopeConstraint, ScopeFilter};
    use modkit_security::pep_properties;
    use owner_entity::ActiveModel;
    use sea_orm::Set;

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let allowed_city = Uuid::new_v4();
    let disallowed_city = Uuid::new_v4();

    // Scope says city_id must be allowed_city
    let scope = AccessScope::from_constraints(vec![ScopeConstraint::new(vec![
        ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![tenant_id]),
        ScopeFilter::eq(pep_properties::OWNER_ID, user_id),
        ScopeFilter::eq("city_id", allowed_city),
    ])]);

    // But ActiveModel has city_id = disallowed_city
    let am = ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        user_id: Set(user_id),
        city_id: Set(disallowed_city),
    };
    assert!(
        validate_insert_scope(&am, &scope).is_err(),
        "Insert must be rejected when city_id doesn't match"
    );
}

#[test]
fn test_validate_insert_scope_or_semantics() {
    use modkit_security::access_scope::{ScopeConstraint, ScopeFilter};
    use modkit_security::pep_properties;
    use owner_entity::ActiveModel;
    use sea_orm::Set;

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let city_1 = Uuid::new_v4();
    let city_2 = Uuid::new_v4();

    // Two constraints (OR-ed): user allowed in city_1 OR city_2
    let scope = AccessScope::from_constraints(vec![
        ScopeConstraint::new(vec![
            ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![tenant_id]),
            ScopeFilter::eq("city_id", city_1),
        ]),
        ScopeConstraint::new(vec![
            ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![tenant_id]),
            ScopeFilter::eq("city_id", city_2),
        ]),
    ]);

    // Insert with city_2 — matches second constraint
    let am = ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        user_id: Set(user_id),
        city_id: Set(city_2),
    };
    assert!(
        validate_insert_scope(&am, &scope).is_ok(),
        "Insert should pass when matching any constraint (OR semantics)"
    );

    // Insert with city_3 — matches neither
    let city_3 = Uuid::new_v4();
    let am_bad = ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        user_id: Set(user_id),
        city_id: Set(city_3),
    };
    assert!(
        validate_insert_scope(&am_bad, &scope).is_err(),
        "Insert must be rejected when no constraint matches"
    );
}

#[test]
fn test_validate_insert_scope_unknown_property_fails_closed() {
    use modkit_security::access_scope::{ScopeConstraint, ScopeFilter};
    use modkit_security::pep_properties;
    use owner_entity::ActiveModel;
    use sea_orm::Set;

    let tenant_id = Uuid::new_v4();

    // Constraint with an unknown property
    let scope = AccessScope::from_constraints(vec![ScopeConstraint::new(vec![
        ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![tenant_id]),
        ScopeFilter::eq("nonexistent_prop", Uuid::new_v4()),
    ])]);

    let am = ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        user_id: Set(Uuid::new_v4()),
        city_id: Set(Uuid::new_v4()),
    };
    assert!(
        validate_insert_scope(&am, &scope).is_err(),
        "Unknown property must cause constraint to fail (fail-closed)"
    );
}
