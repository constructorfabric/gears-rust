use super::{Dimensions, EntityScope, ScopableEntity};
use sea_orm::IdenStatic;
use toolkit_security::pep_properties;

/// Every property name a test entity could answer for.
const PROPERTIES: &[&str] = &[
    pep_properties::OWNER_TENANT_ID,
    pep_properties::RESOURCE_ID,
    pep_properties::OWNER_ID,
    "department_id",
    "kind",
    "type_col",
    "unknown",
];

fn names<C: IdenStatic>(columns: impl Iterator<Item = C>) -> Vec<&'static str> {
    columns.map(|column| column.as_str()).collect()
}

/// What the lookup answers, as `(property, column)` pairs.
fn resolved<C: IdenStatic + Copy>(scope: &EntityScope<C>) -> Vec<(&'static str, &'static str)> {
    PROPERTIES
        .iter()
        .filter_map(|p| scope.resolve_property(p).map(|c| (*p, c.as_str())))
        .collect()
}

/// The lookup and the list describe one set: every column the lookup returns
/// is listed, and every listed column is returned for some property.
fn assert_two_views_agree<C: IdenStatic + Copy>(scope: &EntityScope<C>) {
    let listed = names(scope.scope_columns());
    let found: Vec<_> = resolved(scope).into_iter().map(|(_, c)| c).collect();
    for column in &found {
        assert!(
            listed.contains(column),
            "{column} resolves but is not listed: {listed:?}"
        );
    }
    for column in &listed {
        assert!(
            found.contains(column),
            "{column} is listed but nothing resolves to it: {found:?}"
        );
    }
}

mod derived {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, toolkit_db::secure::Scopable)]
    #[sea_orm(table_name = "derived")]
    #[secure(
        tenant_col = "tenant_id",
        resource_col = "id",
        owner_col = "owner_id",
        type_col = "kind",
        pep_prop(department_id = "department_id")
    )]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub owner_id: Uuid,
        pub kind: String,
        pub department_id: Uuid,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod manual {
    use super::{Dimensions, EntityScope, ScopableEntity};
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "manual")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub department_id: Uuid,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}

    impl ScopableEntity for Entity {
        const SCOPE: EntityScope<Column> = EntityScope::Scoped(Dimensions {
            tenant: Some(Column::TenantId),
            resource: Some(Column::Id),
            owner: None,
            type_col: None,
            custom: &[("department_id", Column::DepartmentId)],
        });
    }
}

mod unrestricted {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, toolkit_db::secure::Scopable)]
    #[sea_orm(table_name = "unrestricted")]
    #[secure(unrestricted)]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod type_only {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, toolkit_db::secure::Scopable)]
    #[sea_orm(table_name = "type_only")]
    #[secure(no_tenant, no_resource, no_owner, type_col = "kind")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub kind: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

#[test]
fn derive_maps_each_attribute_to_its_field() {
    let scope = <derived::Entity as ScopableEntity>::SCOPE;
    assert!(!scope.is_unrestricted());
    assert_eq!(scope.tenant().map(|c| c.as_str()), Some("tenant_id"));
    assert_eq!(scope.resource().map(|c| c.as_str()), Some("id"));
    assert_eq!(scope.owner().map(|c| c.as_str()), Some("owner_id"));
    assert_eq!(scope.type_col().map(|c| c.as_str()), Some("kind"));
    assert_eq!(
        resolved(&scope),
        [
            (pep_properties::OWNER_TENANT_ID, "tenant_id"),
            (pep_properties::RESOURCE_ID, "id"),
            (pep_properties::OWNER_ID, "owner_id"),
            ("department_id", "department_id"),
        ]
    );
    assert_eq!(
        names(scope.scope_columns()),
        ["tenant_id", "id", "owner_id", "department_id"]
    );
}

#[test]
fn lookup_and_list_are_two_views_of_one_scope() {
    assert_two_views_agree(&<derived::Entity as ScopableEntity>::SCOPE);
    assert_two_views_agree(&<manual::Entity as ScopableEntity>::SCOPE);
    assert_two_views_agree(&<unrestricted::Entity as ScopableEntity>::SCOPE);
    assert_two_views_agree(&<type_only::Entity as ScopableEntity>::SCOPE);
}

#[test]
fn the_type_column_is_not_a_scope_column() {
    let scope = <type_only::Entity as ScopableEntity>::SCOPE;
    assert_eq!(scope.type_col().map(|c| c.as_str()), Some("kind"));
    assert!(resolved(&scope).is_empty(), "{:?}", resolved(&scope));
    assert_eq!(scope.scope_columns().count(), 0);
    // It is still a column, so the entity is not column-free.
    assert!(!scope.has_no_columns());
}

#[test]
fn an_unrestricted_entity_has_no_columns_and_resolves_nothing() {
    let scope = <unrestricted::Entity as ScopableEntity>::SCOPE;
    assert!(scope.is_unrestricted());
    assert!(scope.has_no_columns());
    assert!(resolved(&scope).is_empty());
    assert!(scope.tenant().is_none() && scope.resource().is_none());
}

#[test]
fn a_scoped_entity_with_every_dimension_none_is_not_unrestricted() {
    // Link and closure tables: no scope column, yet not global data.
    const SCOPE: EntityScope<manual::Column> = EntityScope::Scoped(Dimensions {
        tenant: None,
        resource: None,
        owner: None,
        type_col: None,
        custom: &[],
    });
    assert!(!SCOPE.is_unrestricted());
    assert!(SCOPE.has_no_columns());
    assert!(resolved(&SCOPE).is_empty());
}

#[test]
fn an_undeclared_property_resolves_to_nothing() {
    let scope = <manual::Entity as ScopableEntity>::SCOPE;
    assert!(scope.resolve_property(pep_properties::OWNER_ID).is_none());
    assert!(scope.resolve_property("unknown").is_none());
}

/// The column iterator does not borrow the scope: `E::SCOPE` is a temporary,
/// so an iterator tied to it could not be bound and used afterwards.
fn columns_of<E: ScopableEntity>() -> Vec<&'static str> {
    let columns = E::SCOPE.scope_columns();
    names(columns)
}

#[test]
fn the_column_iterator_outlives_the_scope_temporary() {
    assert_eq!(
        columns_of::<derived::Entity>(),
        ["tenant_id", "id", "owner_id", "department_id"]
    );
    assert!(columns_of::<unrestricted::Entity>().is_empty());
}
