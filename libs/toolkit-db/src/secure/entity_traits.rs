use sea_orm::EntityTrait;
use toolkit_security::access_scope::pep_properties;

/// Defines the contract for entities that can be scoped by tenant, resource, owner, and type.
///
/// An entity declares three things: the property-to-column table
/// [`SCOPE_PROPERTIES`](Self::SCOPE_PROPERTIES), the dimensions it deliberately
/// leaves out of it ([`UNSCOPED_DIMENSIONS`](Self::UNSCOPED_DIMENSIONS)), and
/// [`type_col()`](Self::type_col).
///
/// The tenant, resource and owner **columns** are not declared separately: they
/// are the well-known properties of that same table, and [`ScopeProperties`]
/// reads them out of it:
/// `tenant_col()` is `resolve_property("owner_tenant_id")`, `resource_col()` is
/// `resolve_property("id")` and `owner_col()` is `resolve_property("owner_id")`.
/// Writing them by hand next to the table would be the same column in two
/// places with nothing checking that they agree, which is the defect this trait
/// was reshaped to remove (issue #4726) — one level up from where it was found.
///
/// [`type_col`](Self::type_col) stays a method of its own because it has no
/// property name: no scope can address it, so it cannot come from the table.
///
/// **Important**: No implicit defaults are allowed. A dimension is scoped when
/// the table names its property and unscoped when
/// [`UNSCOPED_DIMENSIONS`](Self::UNSCOPED_DIMENSIONS) names it; one of the two
/// must, and never both, or the entity does not build. `type_col` must be
/// answered explicitly for the same reason. Silence is not an answer here: the
/// write-side guards that ask for a column skip themselves when there is none,
/// and nothing at runtime can tell a dimension that was decided against from
/// one whose row was forgotten.
///
/// # Example (Manual Implementation)
/// ```rust,ignore
/// impl ScopableEntity for user::Entity {
///     // The property-to-column mapping, written once. `resolve_property`,
///     // `scope_columns` and the three dimension accessors read it (see
///     // `ScopeProperties`) and cannot be implemented per entity, so nothing
///     // that derives from this table can describe a different set.
///     const SCOPE_PROPERTIES: &'static [(&'static str, Self::Column)] = &[
///         (pep_properties::OWNER_TENANT_ID, user::Column::TenantId),
///         (pep_properties::RESOURCE_ID, user::Column::Id),
///     ];
///
///     // And the dimensions the table does not name. Each of the three
///     // belongs to exactly one of the two lists; a dimension in neither is a
///     // build error, because nothing at runtime could tell it from a
///     // dimension the entity genuinely has no column for.
///     const UNSCOPED_DIMENSIONS: &'static [&'static str] = &[pep_properties::OWNER_ID];
///
///     // Tenant and resource are scoped, owner is not — the columns follow
///     // from the table above. Only `type_col` is written out.
///     fn type_col() -> Option<Self::Column> {
///         None
///     }
/// }
/// ```
///
/// # Example (Using Derive Macro)
/// ```rust,ignore
/// use toolkit_db::secure::Scopable;
///
/// #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
/// #[sea_orm(table_name = "users")]
/// #[secure(
///     tenant_col = "tenant_id",
///     resource_col = "id",
///     no_owner,
///     no_type
/// )]
/// pub struct Model {
///     #[sea_orm(primary_key)]
///     pub id: Uuid,
///     pub tenant_id: Uuid,
///     pub email: String,
/// }
/// // Macro auto-generates SCOPE_PROPERTIES:
/// //   ("owner_tenant_id", Column::TenantId)   (from tenant_col)
/// //   ("id",              Column::Id)         (from resource_col)
/// ```
///
/// # Custom PEP Properties
/// ```rust,ignore
/// #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
/// #[sea_orm(table_name = "resources")]
/// #[secure(
///     tenant_col = "tenant_id",
///     resource_col = "id",
///     no_owner,
///     no_type,
///     pep_prop(department_id = "department_id"),
/// )]
/// pub struct Model {
///     #[sea_orm(primary_key)]
///     pub id: Uuid,
///     pub tenant_id: Uuid,
///     pub department_id: Uuid,
/// }
/// // Macro auto-generates SCOPE_PROPERTIES:
/// //   ("owner_tenant_id", Column::TenantId)       (from tenant_col)
/// //   ("id",              Column::Id)             (from resource_col)
/// //   ("department_id",   Column::DepartmentId)   (from pep_prop)
/// ```
///
/// # Unrestricted Entities
/// ```rust,ignore
/// #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
/// #[sea_orm(table_name = "system_config")]
/// #[secure(unrestricted)]
/// pub struct Model {
///     #[sea_orm(primary_key)]
///     pub id: Uuid,
///     pub config_key: String,
/// }
/// ```
pub trait ScopableEntity: EntityTrait {
    /// Indicates whether this entity is explicitly marked as unrestricted.
    ///
    /// This is a compile-time flag set via `#[secure(unrestricted)]` that documents
    /// the entity's global nature (e.g., system configuration, lookup tables).
    ///
    /// When `IS_UNRESTRICTED` is true, all column methods return `None`.
    ///
    /// Default: `false` (entity participates in scoping logic)
    const IS_UNRESTRICTED: bool = false;

    /// Returns the column that stores the resource type identifier.
    ///
    /// Used for type-based filtering in polymorphic scenarios.
    ///
    /// Must be explicitly specified via `type_col = "..."` or `no_type`.
    fn type_col() -> Option<Self::Column>;

    /// Every authorization property this entity understands, paired with the
    /// column that property means.
    ///
    /// The single place the mapping is written.
    /// [`ScopeProperties::resolve_property`] looks one property up in it and
    /// [`ScopeProperties::scope_columns`] lists its columns. Neither can be
    /// implemented per entity — see [`ScopeProperties`] — so the lookup and the
    /// list cannot describe different sets.
    ///
    /// Usually the tenant, resource and owner columns plus any `pep_prop(...)`
    /// columns, keyed by property name.
    /// [`type_col`](Self::type_col) belongs to no entry: no property name
    /// addresses it, so no scope can address it either.
    ///
    /// Two entries may name one column — a `pep_prop` pointing at the tenant
    /// column, say. The table stays as written and
    /// [`ScopeProperties::scope_columns`] then reports that column once per
    /// entry, because the list is a view of the table rather than a set. That
    /// is what the previous, separately-written list did too, and the only
    /// consumer that cares deduplicates: a property-graph declaration adds
    /// each column to an element's `PROPERTIES` once.
    ///
    /// `#[derive(Scopable)]` generates the table from the `#[secure(...)]`
    /// attributes. A manual implementation writes it out:
    ///
    /// ```rust,ignore
    /// const SCOPE_PROPERTIES: &'static [(&'static str, Self::Column)] = &[
    ///     (pep_properties::OWNER_TENANT_ID, Column::TenantId),
    ///     (pep_properties::RESOURCE_ID, Column::Id),
    ///     ("department_id", Column::DepartmentId),
    /// ];
    /// ```
    ///
    /// An unrestricted entity declares an empty table.
    const SCOPE_PROPERTIES: &'static [(&'static str, Self::Column)];

    /// The well-known dimensions this entity deliberately does not scope on.
    ///
    /// Each of `owner_tenant_id`, `id` and `owner_id` must appear either in
    /// [`SCOPE_PROPERTIES`](Self::SCOPE_PROPERTIES) or here, never in both and
    /// never in neither. [`ScopeProperties::DIMENSIONS_ARE_DECLARED`] is the
    /// check, and it fails the build.
    ///
    /// This is not the column written twice that issue #4726 was about: no
    /// column is named here at all. What is named is the *decision* — the
    /// dimensions this entity was asked about and answered "no" to. Without it
    /// a missing row is indistinguishable from a deliberate omission, and the
    /// write-side guards that ask for a column (`tenant_id is required`,
    /// `tenant_id is immutable`) are skipped in silence for both.
    ///
    /// `#[derive(Scopable)]` fills it from `no_tenant`, `no_resource` and
    /// `no_owner`, which it already requires. A manual implementation writes
    /// it out:
    ///
    /// ```rust,ignore
    /// const UNSCOPED_DIMENSIONS: &'static [&'static str] = &[pep_properties::OWNER_ID];
    /// ```
    ///
    /// An unrestricted entity is exempt: it declares an empty table and scopes
    /// on nothing by construction.
    const UNSCOPED_DIMENSIONS: &'static [&'static str];
}

/// The dimensions every entity has to account for, one way or the other.
///
/// `type_col` is absent on purpose: no property name addresses it, so it
/// cannot be in the table, and it is a required method instead.
const WELL_KNOWN_DIMENSIONS: [&str; 3] = [
    pep_properties::OWNER_TENANT_ID,
    pep_properties::RESOURCE_ID,
    pep_properties::OWNER_ID,
];

/// Whether `names` contains `name`, in a `const` context.
const fn names_contain(names: &[&str], name: &str) -> bool {
    let mut i = 0;
    while i < names.len() {
        if property_names_equal(names[i], name) {
            return true;
        }
        i += 1;
    }
    false
}

/// Whether a `SCOPE_PROPERTIES` table has an entry for `property`.
const fn table_names<C>(table: &[(&str, C)], property: &str) -> bool {
    let mut i = 0;
    while i < table.len() {
        if property_names_equal(table[i].0, property) {
            return true;
        }
        i += 1;
    }
    false
}

/// Whether every well-known dimension was decided about exactly once.
///
/// Scoped means the table names it; unscoped means `UNSCOPED_DIMENSIONS` does.
/// Both is a contradiction, neither is an omission, and a name in the list that
/// is not a well-known dimension accounts for nothing — a misspelt
/// `owner_tenat_id` would otherwise read as an answer.
const fn dimensions_are_declared<C>(table: &[(&str, C)], unscoped: &[&str]) -> bool {
    let mut i = 0;
    while i < WELL_KNOWN_DIMENSIONS.len() {
        if table_names(table, WELL_KNOWN_DIMENSIONS[i])
            == names_contain(unscoped, WELL_KNOWN_DIMENSIONS[i])
        {
            return false;
        }
        i += 1;
    }
    let mut j = 0;
    while j < unscoped.len() {
        if !names_contain(&WELL_KNOWN_DIMENSIONS, unscoped[j]) {
            return false;
        }
        j += 1;
    }
    true
}

/// `a == b` for two `&str`, in a `const` context.
///
/// `str`'s own `PartialEq` is not `const`, and the check below has to run at
/// compile time to be worth anything.
const fn property_names_equal(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Whether a `SCOPE_PROPERTIES` table names every property at most once.
///
/// Quadratic, over a table of a handful of entries, at compile time.
const fn properties_are_unique<C>(table: &[(&str, C)]) -> bool {
    let mut i = 0;
    while i < table.len() {
        let mut j = i + 1;
        while j < table.len() {
            if property_names_equal(table[i].0, table[j].0) {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}

/// Everything read out of [`ScopableEntity::SCOPE_PROPERTIES`]: look one
/// property up, list the columns, or ask for one of the three well-known
/// dimensions.
///
/// Separate from [`ScopableEntity`] on purpose. The blanket implementation
/// below covers every scopable entity, so an entity **cannot** provide its own
/// version of any of them: coherence rejects a second implementation
/// (`E0119`). That is what makes "everything here describes one set" an
/// invariant of the type system rather than a convention — the reason this
/// trait exists at all is that when the lookup and the list were both
/// overridable methods of `ScopableEntity`, a hand-written entity could and did
/// make them disagree (issue #4726). The dimension accessors are here for the
/// same reason: declared beside the table they were one column written twice,
/// with nothing checking that the two spellings agreed.
///
/// Callers get it for free with `E: ScopableEntity`; the trait only has to be
/// in scope:
///
/// ```rust,ignore
/// use toolkit_db::secure::{ScopableEntity, ScopeProperties};
///
/// fn column_for<E: ScopableEntity>(property: &str) -> Option<E::Column> {
///     E::resolve_property(property)
/// }
/// ```
pub trait ScopeProperties: ScopableEntity {
    /// Compile-time proof that the table names no property twice.
    ///
    /// `resolve_property` answers with the first matching entry, so a repeated
    /// property name would make the later entries unreachable and silently
    /// decide which column a scope constraint lands on. The `match` this lookup
    /// replaced got an `unreachable_patterns` warning for that mistake; this
    /// const puts the check back, and unlike the warning it covers hand-written
    /// tables as well as derived ones.
    ///
    /// An associated const is only evaluated where it is used, so the two
    /// readers below force it with a `const` block. An entity whose table is
    /// never read is never checked — and never scopes anything either.
    ///
    /// `Self` is generic here, so the assertion is evaluated at monomorphization:
    /// it fails a build (`cargo build`, `cargo test`, and every CI job that
    /// compiles) but **not** a `cargo check`, which stops at metadata. That is
    /// also why this cannot be a `trybuild` fixture — trybuild runs `cargo check`.
    const PROPERTIES_ARE_UNIQUE: () = assert!(
        properties_are_unique(Self::SCOPE_PROPERTIES),
        "SCOPE_PROPERTIES names one property twice: resolve_property would answer \
         with the first entry and quietly ignore the rest"
    );

    /// Compile-time proof that every well-known dimension was decided about.
    ///
    /// The three dimension accessors below answer `None` both for an entity
    /// that has no such column and for one whose table forgot the row, and the
    /// write-side guards skip themselves on `None` either way. Nothing at
    /// runtime can tell the two apart, so the decision is made a declaration
    /// and checked here.
    ///
    /// Forced, evaluated and unavailable to `trybuild` for the same reasons as
    /// [`PROPERTIES_ARE_UNIQUE`](Self::PROPERTIES_ARE_UNIQUE); see there.
    const DIMENSIONS_ARE_DECLARED: () = assert!(
        Self::IS_UNRESTRICTED
            || dimensions_are_declared(Self::SCOPE_PROPERTIES, Self::UNSCOPED_DIMENSIONS),
        "every one of `owner_tenant_id`, `id` and `owner_id` must be named by \
         SCOPE_PROPERTIES or by UNSCOPED_DIMENSIONS, and by exactly one of \
         them: a dimension in neither is one nobody decided about, and the \
         write guards that ask for its column would skip themselves in silence"
    );

    /// Resolve an authorization property name to a database column.
    ///
    /// Maps PEP property names (e.g. `"owner_tenant_id"`) to `SeaORM` columns
    /// so the scope condition builder can translate `AccessScope` constraints
    /// into SQL `WHERE` clauses.
    ///
    /// A property the entity does not declare resolves to `None`. That is a
    /// runtime answer, not a compile-time guarantee: it is on the caller to
    /// treat it as deny. The compilers of this crate's own callers do so by
    /// dropping the constraint and falling to `WHERE false`.
    #[must_use]
    fn resolve_property(property: &str) -> Option<Self::Column> {
        const { Self::PROPERTIES_ARE_UNIQUE }
        const { Self::DIMENSIONS_ARE_DECLARED }
        Self::SCOPE_PROPERTIES
            .iter()
            .find(|(name, _)| *name == property)
            .map(|(_, column)| *column)
    }

    /// The columns a scope predicate can be compiled against: the columns of
    /// [`ScopableEntity::SCOPE_PROPERTIES`], in declaration order.
    ///
    /// One entry, one column, in order: a column two properties both name is
    /// reported twice. [`resolve_property`](Self::resolve_property) answers for
    /// one property and cannot be enumerated, and the SQL/PGQ graph declaration
    /// needs the whole set. A scope column left out of an element's `PROPERTIES` list
    /// cannot be filtered on inside `MATCH` (`docs/arch/secure-orm/ADR/0002`,
    /// Policy 3), and an element whose set is empty is refused up front rather
    /// than compiling to a deny-all traversal (Policy 2).
    #[must_use]
    fn scope_columns() -> Vec<Self::Column> {
        const { Self::PROPERTIES_ARE_UNIQUE }
        const { Self::DIMENSIONS_ARE_DECLARED }
        Self::SCOPE_PROPERTIES
            .iter()
            .map(|(_, column)| *column)
            .collect()
    }

    /// The column that stores the tenant identifier, or `None` when the entity
    /// is not tenant-scoped.
    ///
    /// Read out of [`ScopableEntity::SCOPE_PROPERTIES`] under the well-known
    /// property name `owner_tenant_id`, rather than declared beside it. An
    /// entity that names that property is tenant-scoped, by the same fact that
    /// makes a PDP constraint on `owner_tenant_id` compile to a predicate on
    /// this column — so the two cannot disagree.
    #[must_use]
    fn tenant_col() -> Option<Self::Column> {
        Self::resolve_property(pep_properties::OWNER_TENANT_ID)
    }

    /// The column that stores the primary resource identifier, or `None` when
    /// the entity is not resource-scoped.
    ///
    /// Typically the primary key. Read out of the table under `id`, as
    /// [`tenant_col`](Self::tenant_col) is.
    #[must_use]
    fn resource_col() -> Option<Self::Column> {
        Self::resolve_property(pep_properties::RESOURCE_ID)
    }

    /// The column that stores the resource owner identifier, or `None` when the
    /// entity is not owner-scoped.
    ///
    /// Read out of the table under `owner_id`, as
    /// [`tenant_col`](Self::tenant_col) is.
    #[must_use]
    fn owner_col() -> Option<Self::Column> {
        Self::resolve_property(pep_properties::OWNER_ID)
    }
}

/// Every scopable entity, and no room for a second implementation.
///
/// `tests/ui/fail/scope_properties_cannot_be_overridden.rs` pins that: an
/// entity trying to supply its own `resolve_property` fails to compile.
impl<E: ScopableEntity> ScopeProperties for E {}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::{ScopableEntity, ScopeProperties};
    use sea_orm::IdenStatic as _;
    use toolkit_security::access_scope::pep_properties;

    /// `DeriveEntityModel` does not derive `PartialEq` on `Column`, so columns
    /// are compared by the name they render as — which is also what the graph
    /// declaration writes into `PROPERTIES`.
    fn table_of<E: ScopableEntity>() -> Vec<(&'static str, &'static str)> {
        E::SCOPE_PROPERTIES
            .iter()
            .map(|(property, column)| (*property, column.as_str()))
            .collect()
    }

    fn listed_columns<E: ScopableEntity>() -> Vec<&'static str> {
        E::scope_columns()
            .iter()
            .map(sea_orm::IdenStatic::as_str)
            .collect()
    }

    /// Every dimension the derive maps, plus a custom property, so the table
    /// under test is not the trivial one.
    mod derived {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, toolkit_db_macros::Scopable)]
        #[sea_orm(table_name = "scope_properties_derived")]
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

    mod unrestricted {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, toolkit_db_macros::Scopable)]
        #[sea_orm(table_name = "scope_properties_unrestricted")]
        #[secure(unrestricted)]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub config_key: String,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    /// A hand-written implementation, written the way the trait documents.
    /// Present so the property below is checked on the shape a gear writes,
    /// not only on the derive's output.
    mod manual {
        use sea_orm::entity::prelude::*;
        use toolkit_security::access_scope::pep_properties;

        #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
        #[sea_orm(table_name = "scope_properties_manual")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub tenant_id: Uuid,
            pub department_id: Uuid,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}

        impl crate::secure::ScopableEntity for Entity {
            const SCOPE_PROPERTIES: &'static [(&'static str, Self::Column)] = &[
                (pep_properties::OWNER_TENANT_ID, Column::TenantId),
                (pep_properties::RESOURCE_ID, Column::Id),
                ("department_id", Column::DepartmentId),
            ];

            const UNSCOPED_DIMENSIONS: &'static [&'static str] = &[pep_properties::OWNER_ID];

            fn type_col() -> Option<Column> {
                None
            }
        }
    }

    /// What the derive builds out of `#[secure(...)]`: one entry per dimension
    /// that names a column, plus one per `pep_prop`, keyed by property name.
    #[test]
    fn the_derive_builds_the_table_from_the_attributes() {
        assert_eq!(
            table_of::<derived::Entity>(),
            vec![
                (pep_properties::OWNER_TENANT_ID, "tenant_id"),
                (pep_properties::RESOURCE_ID, "id"),
                (pep_properties::OWNER_ID, "owner_id"),
                ("department_id", "department_id"),
            ]
        );
    }

    /// The property this change exists for (#4726): the lookup and the list are
    /// two views of one table, so an entity cannot answer for a property whose
    /// column the list omits, nor list a column no property resolves to.
    ///
    /// Checked for the derive and for a hand-written implementation, because
    /// those were the two places that could drift when the two were written
    /// separately.
    #[test]
    fn the_lookup_and_the_list_are_two_views_of_one_table() {
        fn check<E: ScopableEntity>(what: &str) {
            let table = table_of::<E>();
            let listed = listed_columns::<E>();
            assert_eq!(
                listed.len(),
                table.len(),
                "{what}: the list must have one column per table entry"
            );
            for (index, (property, column)) in table.iter().enumerate() {
                assert_eq!(
                    E::resolve_property(property).map(|c| c.as_str()),
                    Some(*column),
                    "{what}: the lookup must answer every property the table declares"
                );
                assert_eq!(
                    listed[index], *column,
                    "{what}: the list must carry every column the table declares"
                );
            }
        }

        check::<derived::Entity>("derived");
        check::<manual::Entity>("manual");
        // `unrestricted::Entity` is deliberately not walked here: its table is
        // empty, so the loop runs zero times and the only surviving assertion
        // is `0 == 0`, which nothing can make fail.
        // `an_unrestricted_entity_declares_an_empty_table` covers that case.
    }

    /// A column two properties both name is reported once per entry, and the
    /// lookup answers for both names.
    ///
    /// The list is a view of the table, not a set — which is what the
    /// separately-written list did before this change too. The only consumer
    /// that cares deduplicates: a property-graph declaration adds each column
    /// to an element's `PROPERTIES` once, so a repeat costs nothing there.
    #[test]
    fn a_column_two_properties_name_is_listed_per_entry() {
        mod shared_column {
            use sea_orm::entity::prelude::*;

            // `nickname` is a second property for the tenant column.
            #[derive(
                Clone, Debug, PartialEq, Eq, DeriveEntityModel, toolkit_db_macros::Scopable,
            )]
            #[sea_orm(table_name = "scope_properties_shared_column")]
            #[secure(
                tenant_col = "tenant_id",
                no_resource,
                no_owner,
                no_type,
                pep_prop(nickname = "tenant_id")
            )]
            pub struct Model {
                #[sea_orm(primary_key, auto_increment = false)]
                pub id: Uuid,
                pub tenant_id: Uuid,
            }

            #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
            pub enum Relation {}

            impl ActiveModelBehavior for ActiveModel {}
        }

        assert_eq!(
            table_of::<shared_column::Entity>(),
            vec![
                (pep_properties::OWNER_TENANT_ID, "tenant_id"),
                ("nickname", "tenant_id"),
            ]
        );
        assert_eq!(
            listed_columns::<shared_column::Entity>(),
            vec!["tenant_id", "tenant_id"],
            "one column per entry, in order"
        );
        for property in [pep_properties::OWNER_TENANT_ID, "nickname"] {
            assert_eq!(
                shared_column::Entity::resolve_property(property).map(|c| c.as_str()),
                Some("tenant_id"),
                "both names must resolve to the shared column"
            );
        }
    }

    /// A property no entry names resolves to nothing, which every caller reads
    /// as fail-closed.
    #[test]
    fn an_undeclared_property_resolves_to_nothing() {
        assert!(derived::Entity::resolve_property("no_such_property").is_none());
        assert!(manual::Entity::resolve_property("no_such_property").is_none());
    }

    /// `type_col` is a dimension but not a scope property: no property name
    /// addresses it, so a scope cannot either. Were it in the table, an entity
    /// whose only dimension is `type_col` would pass the Policy 2 gates while
    /// resolving nothing (`docs/arch/secure-orm/ADR/0002`).
    #[test]
    fn the_type_column_is_not_a_scope_property() {
        assert_eq!(
            derived::Entity::type_col().map(|c| c.as_str()),
            Some("kind"),
            "the fixture must declare a type column for this to mean anything"
        );
        assert!(
            !table_of::<derived::Entity>()
                .iter()
                .any(|(_, column)| *column == "kind"),
            "the type column must not be addressable as a property"
        );
        assert!(!listed_columns::<derived::Entity>().contains(&"kind"));
    }

    /// An unrestricted entity has nothing to scope by, and says so with one
    /// empty table rather than with two separately-written empty answers.
    #[test]
    fn an_unrestricted_entity_declares_an_empty_table() {
        use unrestricted::Entity;

        const { assert!(unrestricted::Entity::IS_UNRESTRICTED) };
        assert!(Entity::SCOPE_PROPERTIES.is_empty());
        assert!(Entity::scope_columns().is_empty());
        assert!(Entity::resolve_property(pep_properties::OWNER_TENANT_ID).is_none());
    }

    /// The uniqueness rule `PROPERTIES_ARE_UNIQUE` asserts, tested directly.
    ///
    /// The assertion itself is evaluated at monomorphization, so a table that
    /// breaks it fails a build rather than a test — there is no way to assert
    /// on it from inside a test that has to compile. What a test can pin is the
    /// rule the assertion applies, which is what these do.
    mod uniqueness_rule {
        use super::super::{properties_are_unique, property_names_equal};

        #[test]
        fn a_repeated_property_name_is_rejected() {
            // Two entries under one name: `resolve_property` would answer with
            // the first and the second would be dead, so which column the
            // tenant predicate lands on would be decided by writing order.
            assert!(!properties_are_unique(&[
                ("owner_tenant_id", 1),
                ("id", 2),
                ("owner_tenant_id", 3),
            ]));
        }

        #[test]
        fn a_repeated_column_under_two_names_is_allowed() {
            // The documented, intentional case: one column, two property names.
            // Only the names have to be unique.
            assert!(properties_are_unique(&[
                ("owner_tenant_id", 1),
                ("nickname", 1),
            ]));
        }

        #[test]
        fn an_empty_or_single_entry_table_is_unique() {
            assert!(properties_are_unique::<u8>(&[]));
            assert!(properties_are_unique(&[("id", 1)]));
        }

        #[test]
        fn names_compare_by_content_not_by_pointer() {
            // One side owns its bytes, so the two arguments cannot be the same
            // static address. A comparison by pointer would answer `false`
            // here, which is what the name of this test promises it rules out.
            let owned = String::from("owner_tenant_id");
            assert!(property_names_equal(&owned, "owner_tenant_id"));
            assert!(!property_names_equal("owner_tenant_id", "owner_id"));
            // A prefix is not a match: the length is checked first.
            assert!(!property_names_equal("owner", "owner_id"));
        }
    }

    /// The rule [`ScopeProperties::DIMENSIONS_ARE_DECLARED`] enforces.
    ///
    /// Tested here rather than as a `trybuild` fixture for the reason that
    /// const documents: the assertion is evaluated at monomorphization, and
    /// `trybuild` runs `cargo check`, which stops at metadata.
    mod dimension_rule {
        use super::super::dimensions_are_declared;
        use toolkit_security::access_scope::pep_properties;

        /// A table naming all three, and nothing left to declare.
        #[test]
        fn a_fully_scoped_entity_declares_nothing_unscoped() {
            assert!(dimensions_are_declared(
                &[
                    (pep_properties::OWNER_TENANT_ID, 1),
                    (pep_properties::RESOURCE_ID, 2),
                    (pep_properties::OWNER_ID, 3),
                ],
                &[]
            ));
        }

        /// The complement: nothing scoped, all three declared unscoped. A link
        /// table with no identity of its own is the real shape.
        #[test]
        fn an_entity_that_scopes_on_nothing_declares_all_three() {
            assert!(dimensions_are_declared::<u8>(
                &[],
                &[
                    pep_properties::OWNER_TENANT_ID,
                    pep_properties::RESOURCE_ID,
                    pep_properties::OWNER_ID,
                ]
            ));
        }

        /// The regression this rule exists for: a dimension in neither place.
        /// `tenant_col()` answers `None`, and every write-side guard that asks
        /// for the column skips itself, exactly as it would for an entity that
        /// has no tenant at all.
        #[test]
        fn a_dimension_in_neither_place_is_rejected() {
            assert!(!dimensions_are_declared(
                &[
                    (pep_properties::RESOURCE_ID, 2),
                    (pep_properties::OWNER_ID, 3),
                ],
                &[]
            ));
        }

        /// And the contradiction: scoped and unscoped at once. The table would
        /// win silently, so the declaration would be a lie nobody reads.
        #[test]
        fn a_dimension_in_both_places_is_rejected() {
            assert!(!dimensions_are_declared(
                &[
                    (pep_properties::OWNER_TENANT_ID, 1),
                    (pep_properties::RESOURCE_ID, 2),
                    (pep_properties::OWNER_ID, 3),
                ],
                &[pep_properties::OWNER_ID]
            ));
        }

        /// A name that is not a dimension accounts for nothing. Without this,
        /// `owner_tenat_id` would leave the tenant undeclared while looking
        /// like an answer -- the same silence the rule is here to end.
        #[test]
        fn a_name_that_is_not_a_dimension_is_rejected() {
            assert!(!dimensions_are_declared(
                &[(pep_properties::RESOURCE_ID, 2)],
                &["owner_tenat_id", pep_properties::OWNER_ID]
            ));
        }

        /// A `pep_prop` entry is not a dimension and must not stand in for one.
        #[test]
        fn a_pep_prop_does_not_answer_for_a_dimension() {
            assert!(!dimensions_are_declared(
                &[
                    (pep_properties::RESOURCE_ID, 2),
                    ("department_id", 4),
                    (pep_properties::OWNER_ID, 3),
                ],
                &[]
            ));
        }
    }
}
