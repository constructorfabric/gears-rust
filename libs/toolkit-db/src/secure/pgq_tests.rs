//! Tests for the property-graph declaration.
//!
//! What is asserted here is not syntax — the syntax layer has its own tests —
//! but the three guarantees the declaration exists to provide: scope columns
//! reach `PROPERTIES` without the caller repeating them, an element that cannot
//! be scoped is refused, and a label cannot be shared.

use super::pgq::{Endpoint, GraphDeclaration, PropertyGraph, VertexOf};
use crate::secure::{ScopableEntity, ScopeError};

/// A vertex with the usual tenant/resource dimensions.
mod node {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "graph_node")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i64,
        pub name: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}

    impl crate::secure::ScopableEntity for Entity {
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
            use toolkit_security::access_scope::pep_properties;
            match property {
                p if p == pep_properties::OWNER_TENANT_ID => Some(Column::TenantId),
                p if p == pep_properties::RESOURCE_ID => Some(Column::Id),
                _ => None,
            }
        }
    }
}

/// An edge that carries a tenant, as a secure graph element must.
mod edge {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "graph_edge")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i64,
        pub src_node_id: i64,
        pub dst_node_id: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}

    impl crate::secure::ScopableEntity for Entity {
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
            use toolkit_security::access_scope::pep_properties;
            match property {
                p if p == pep_properties::OWNER_TENANT_ID => Some(Column::TenantId),
                p if p == pep_properties::RESOURCE_ID => Some(Column::Id),
                _ => None,
            }
        }
    }
}

/// A vertex whose scope columns are **not** all key columns. Without one of
/// these, a test asserting "the scope columns reached PROPERTIES" passes even if
/// the list was built from the key alone, because the two sets coincide.
mod owned {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "owned_thing")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i64,
        pub tenant_id: Uuid,
        pub owner_id: Uuid,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}

    impl crate::secure::ScopableEntity for Entity {
        fn tenant_col() -> Option<Column> {
            Some(Column::TenantId)
        }
        fn resource_col() -> Option<Column> {
            Some(Column::Id)
        }
        fn owner_col() -> Option<Column> {
            Some(Column::OwnerId)
        }
        fn type_col() -> Option<Column> {
            None
        }
        fn resolve_property(property: &str) -> Option<Column> {
            use toolkit_security::access_scope::pep_properties;
            match property {
                p if p == pep_properties::OWNER_TENANT_ID => Some(Column::TenantId),
                p if p == pep_properties::RESOURCE_ID => Some(Column::Id),
                p if p == pep_properties::OWNER_ID => Some(Column::OwnerId),
                _ => None,
            }
        }
    }
}

/// A closure table, shaped exactly like the platform's real ones: a pair of
/// foreign keys and no scope dimension of its own. Correct for that table and
/// consequently ineligible as a graph element.
mod closure {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "tenant_closure")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub ancestor_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub descendant_id: Uuid,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}

    impl crate::secure::ScopableEntity for Entity {
        fn tenant_col() -> Option<Column> {
            None
        }
        fn resource_col() -> Option<Column> {
            None
        }
        fn owner_col() -> Option<Column> {
            None
        }
        fn type_col() -> Option<Column> {
            None
        }
        fn resolve_property(_property: &str) -> Option<Column> {
            None
        }
    }
}

struct Kb;

impl PropertyGraph for Kb {
    const GRAPH_NAME: &'static str = "kb_pgq";

    fn declaration() -> Result<GraphDeclaration, ScopeError> {
        GraphDeclaration::new::<Self>()
            .vertex::<Self, node::Entity>(&["tenant_id", "id"])?
            .edge::<Self, edge::Entity>(
                &["tenant_id", "id"],
                Endpoint {
                    key: vec!["tenant_id".to_owned(), "src_node_id".to_owned()],
                    table: "graph_node".to_owned(),
                    references: vec!["tenant_id".to_owned(), "id".to_owned()],
                },
                Endpoint {
                    key: vec!["tenant_id".to_owned(), "dst_node_id".to_owned()],
                    table: "graph_node".to_owned(),
                    references: vec!["tenant_id".to_owned(), "id".to_owned()],
                },
            )
    }
}

impl VertexOf<Kb> for node::Entity {
    const LABEL: &'static str = "node";
}

impl super::pgq::EdgeOf<Kb> for edge::Entity {
    const LABEL: &'static str = "edge";
}

/// The caller listed only the key columns. The scope columns arrive because the
/// declaration reads them off the entity — which is the whole point: a scope
/// column absent from `PROPERTIES` is invisible to `MATCH`, silently.
#[test]
fn scope_columns_reach_the_properties_list_without_being_repeated() {
    let declaration = Kb::declaration().expect("declares");
    let node_props = declaration.properties_of("node").expect("node is declared");
    assert!(node_props.contains(&"tenant_id".to_owned()));
    assert!(node_props.contains(&"id".to_owned()));
    // And nothing that is not a key or a scope column: a wider list is not
    // wrong, but it would mean the source of the list is not the entity.
    assert_eq!(node_props.len(), 2, "{node_props:?}");
}

/// The distinguishing case: a scope column that is not a key column must still
/// reach `PROPERTIES`. Asserted separately because in an entity whose key *is*
/// its scope columns, a list built from the key alone looks identical — a test
/// that only covers that shape passes against a declaration that ignores the
/// entity entirely.
#[test]
fn a_scope_column_outside_the_key_still_reaches_the_properties_list() {
    struct Owned;
    impl PropertyGraph for Owned {
        const GRAPH_NAME: &'static str = "owned";
        fn declaration() -> Result<GraphDeclaration, ScopeError> {
            // Keyed on `id` only; `tenant_id` and `owner_id` are scope columns
            // that appear nowhere in the key.
            GraphDeclaration::new::<Self>().vertex::<Self, owned::Entity>(&["id"])
        }
    }
    impl VertexOf<Owned> for owned::Entity {
        const LABEL: &'static str = "owned";
    }

    let declaration = Owned::declaration().expect("declares");
    let props = declaration.properties_of("owned").expect("declared");
    assert!(props.contains(&"id".to_owned()), "{props:?}");
    assert!(
        props.contains(&"tenant_id".to_owned()),
        "the tenant column must be filterable inside a pattern: {props:?}"
    );
    assert!(
        props.contains(&"owner_id".to_owned()),
        "the owner column must be filterable inside a pattern: {props:?}"
    );
}

/// The edge is scoped too. An edge table that carried no tenant would be
/// ineligible, which is the modelling constraint Policy 2 puts on gears.
#[test]
fn the_edge_exposes_its_scope_columns_as_well() {
    let declaration = Kb::declaration().expect("declares");
    let props = declaration.properties_of("edge").expect("edge is declared");
    assert!(props.contains(&"tenant_id".to_owned()), "{props:?}");
}

/// The failure this refusal prevents is silent: such an element compiles to
/// `WHERE false` under any constrained scope, so the traversal returns nothing
/// and looks like missing data.
#[test]
fn an_element_that_resolves_no_scope_column_is_refused() {
    struct Bad;
    impl PropertyGraph for Bad {
        const GRAPH_NAME: &'static str = "bad";
        fn declaration() -> Result<GraphDeclaration, ScopeError> {
            GraphDeclaration::new::<Self>().vertex::<Self, closure::Entity>(&["ancestor_id"])
        }
    }
    impl VertexOf<Bad> for closure::Entity {
        const LABEL: &'static str = "closure";
    }

    let err = Bad::declaration().expect_err("a closure table cannot be a graph element");
    assert!(
        matches!(err, ScopeError::Invalid(msg) if msg.contains("at least one scope column")),
        "unexpected error: {err}"
    );
}

/// Sharing a label across element tables is legal SQL/PGQ and unsafe here:
/// security is decided per entity, so one label would carry several mappings.
#[test]
fn two_elements_cannot_share_a_label() {
    struct Shared;
    impl PropertyGraph for Shared {
        const GRAPH_NAME: &'static str = "shared";
        fn declaration() -> Result<GraphDeclaration, ScopeError> {
            GraphDeclaration::new::<Self>()
                .vertex::<Self, node::Entity>(&["tenant_id", "id"])?
                .vertex::<Self, edge::Entity>(&["tenant_id", "id"])
        }
    }
    impl VertexOf<Shared> for node::Entity {
        const LABEL: &'static str = "thing";
    }
    impl VertexOf<Shared> for edge::Entity {
        const LABEL: &'static str = "thing";
    }

    let err = Shared::declaration().expect_err("a shared label must be refused");
    assert!(
        matches!(err, ScopeError::Invalid(msg) if msg.contains("share a label")),
        "unexpected error: {err}"
    );
}

/// An element with no key cannot be referenced by an edge, so it is refused
/// rather than declared and discovered later.
#[test]
fn an_element_without_a_key_is_refused() {
    struct NoKey;
    impl PropertyGraph for NoKey {
        const GRAPH_NAME: &'static str = "nokey";
        fn declaration() -> Result<GraphDeclaration, ScopeError> {
            GraphDeclaration::new::<Self>().vertex::<Self, node::Entity>(&[])
        }
    }
    impl VertexOf<NoKey> for node::Entity {
        const LABEL: &'static str = "node";
    }
    assert!(NoKey::declaration().is_err());
}

/// The composite endpoint key is what makes an edge structurally unable to join
/// a vertex of another tenant, before any scope predicate is applied — so the
/// DDL must actually carry it.
#[test]
fn the_rendered_ddl_carries_composite_endpoint_keys() {
    let sql = Kb::declaration()
        .expect("declares")
        .create_statement()
        .expect("renders");
    assert!(sql.contains(r#"CREATE PROPERTY GRAPH "kb_pgq""#), "{sql}");
    assert!(
        sql.contains(
            r#"SOURCE KEY ("tenant_id", "src_node_id") REFERENCES "graph_node" ("tenant_id", "id")"#
        ),
        "{sql}"
    );
    assert!(
        sql.contains(r#"LABEL "node" PROPERTIES ("tenant_id", "id")"#),
        "{sql}"
    );
}

#[test]
fn the_drop_statement_matches_the_declared_name() {
    let sql = Kb::declaration()
        .expect("declares")
        .drop_statement()
        .expect("renders");
    assert_eq!(sql, r#"DROP PROPERTY GRAPH IF EXISTS "kb_pgq""#);
}

/// An endpoint that carries two columns and references one would produce a DDL
/// `PostgreSQL` rejects, so it is caught here where the message can be useful.
#[test]
fn a_mismatched_endpoint_arity_is_refused() {
    struct Mismatched;
    impl PropertyGraph for Mismatched {
        const GRAPH_NAME: &'static str = "mismatched";
        fn declaration() -> Result<GraphDeclaration, ScopeError> {
            GraphDeclaration::new::<Self>()
                .vertex::<Self, node::Entity>(&["tenant_id", "id"])?
                .edge::<Self, edge::Entity>(
                    &["tenant_id", "id"],
                    Endpoint {
                        key: vec!["tenant_id".to_owned(), "src_node_id".to_owned()],
                        table: "graph_node".to_owned(),
                        references: vec!["id".to_owned()],
                    },
                    Endpoint {
                        key: vec!["tenant_id".to_owned(), "dst_node_id".to_owned()],
                        table: "graph_node".to_owned(),
                        references: vec!["tenant_id".to_owned(), "id".to_owned()],
                    },
                )
        }
    }
    impl VertexOf<Mismatched> for node::Entity {
        const LABEL: &'static str = "node";
    }
    impl super::pgq::EdgeOf<Mismatched> for edge::Entity {
        const LABEL: &'static str = "edge";
    }

    let err = Mismatched::declaration().expect_err("arity must match");
    assert!(
        matches!(err, ScopeError::Invalid(msg) if msg.contains("as many columns")),
        "unexpected error: {err}"
    );
}

/// `scope_columns` has to include `pep_prop` entries, because `resolve_property`
/// can resolve them and therefore a scope can address them - so a pattern must
/// be able to filter on them. Only the derive knows that set; the trait's
/// default cannot.
#[test]
fn the_derive_enumerates_custom_pep_properties() {
    mod derived {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, toolkit_db_macros::Scopable)]
        #[sea_orm(table_name = "with_custom")]
        #[secure(
            tenant_col = "tenant_id",
            resource_col = "id",
            no_owner,
            no_type,
            pep_prop(department_id = "department_id")
        )]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub tenant_id: Uuid,
            pub department_id: Uuid,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    let names: Vec<&str> = <derived::Entity as ScopableEntity>::scope_columns()
        .iter()
        .map(sea_orm::IdenStatic::as_str)
        .collect();
    assert!(names.contains(&"tenant_id"), "{names:?}");
    assert!(names.contains(&"id"), "{names:?}");
    assert!(
        names.contains(&"department_id"),
        "a pep_prop column is a scope column: {names:?}"
    );
}
