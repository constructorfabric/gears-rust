use sea_orm::EntityTrait;
use toolkit_security::pep_properties;

/// How an entity takes part in access scoping.
///
/// An entity is either [`Unrestricted`](Self::Unrestricted) — global data that
/// no scope narrows — or [`Scoped`](Self::Scoped) by its four named
/// [`Dimensions`] and any custom PEP properties.
///
/// Every reading of the scope goes through the methods here, so the property
/// lookup ([`resolve_property`](Self::resolve_property)) and the column list
/// ([`scope_columns`](Self::scope_columns)) are two views of one value and
/// cannot disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntityScope<C: 'static> {
    /// Global data: no column is filtered, every scope admits every row.
    Unrestricted,
    /// Scoped by the given dimensions. An entity with every dimension `None`
    /// and no custom property is still scoped: a real scope resolves nothing
    /// on it and denies.
    Scoped(Dimensions<C>),
}

/// The scope dimensions of a [`Scoped`](EntityScope::Scoped) entity.
///
/// Each named dimension is either a column or `None`, and every field must be
/// written out: a forgotten dimension is a missing-field error at the entity.
/// For that reason there is deliberately no `Default` and no ready-made
/// "all `None`" value — with one, a partly filled struct would compile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dimensions<C: 'static> {
    /// Tenant column, addressed by the `owner_tenant_id` property.
    pub tenant: Option<C>,
    /// Resource identity column (usually the primary key), addressed by `id`.
    pub resource: Option<C>,
    /// Owner column, addressed by `owner_id`.
    pub owner: Option<C>,
    /// Type column for polymorphic tables. No property name addresses it, so
    /// it is not a scope column.
    pub type_col: Option<C>,
    /// Custom PEP properties as `(property, column)`. Their names must not
    /// repeat each other or a well-known property; `#[derive(Scopable)]`
    /// checks both.
    pub custom: &'static [(&'static str, C)],
}

impl<C: Copy + 'static> EntityScope<C> {
    /// `true` for [`EntityScope::Unrestricted`].
    #[must_use]
    pub const fn is_unrestricted(&self) -> bool {
        matches!(self, Self::Unrestricted)
    }

    /// The tenant column, if the entity has one.
    #[must_use]
    pub const fn tenant(&self) -> Option<C> {
        match self {
            Self::Unrestricted => None,
            Self::Scoped(dims) => dims.tenant,
        }
    }

    /// The resource identity column, if the entity has one.
    #[must_use]
    pub const fn resource(&self) -> Option<C> {
        match self {
            Self::Unrestricted => None,
            Self::Scoped(dims) => dims.resource,
        }
    }

    /// The owner column, if the entity has one.
    #[must_use]
    pub const fn owner(&self) -> Option<C> {
        match self {
            Self::Unrestricted => None,
            Self::Scoped(dims) => dims.owner,
        }
    }

    /// The type column, if the entity has one.
    #[must_use]
    pub const fn type_col(&self) -> Option<C> {
        match self {
            Self::Unrestricted => None,
            Self::Scoped(dims) => dims.type_col,
        }
    }

    /// Resolve an authorization property name to a column.
    ///
    /// `owner_tenant_id`, `id` and `owner_id` resolve to the tenant, resource
    /// and owner columns; any other name is looked up among the custom
    /// properties. The type column has no property name and is never
    /// returned. An unrestricted entity resolves nothing.
    #[must_use]
    pub fn resolve_property(&self, property: &str) -> Option<C> {
        let Self::Scoped(dims) = self else {
            return None;
        };
        match property {
            pep_properties::OWNER_TENANT_ID => dims.tenant,
            pep_properties::RESOURCE_ID => dims.resource,
            pep_properties::OWNER_ID => dims.owner,
            _ => dims
                .custom
                .iter()
                .find(|(name, _)| *name == property)
                .map(|(_, column)| *column),
        }
    }

    /// The columns a scope predicate can be compiled against: exactly the set
    /// [`resolve_property`](Self::resolve_property) can return.
    ///
    /// Tenant, resource and owner columns, then the custom ones. The type
    /// column is not included, since no property resolves to it. A column two
    /// properties name appears twice. Empty for an unrestricted entity.
    ///
    /// The iterator does not borrow `self`: it holds copies of the three
    /// named columns and the `'static` custom list, so it outlives a
    /// temporary such as `E::SCOPE`.
    pub fn scope_columns(&self) -> impl Iterator<Item = C> + use<C> {
        let (named, custom) = match self {
            Self::Unrestricted => ([None; 3], &[][..]),
            Self::Scoped(dims) => ([dims.tenant, dims.resource, dims.owner], dims.custom),
        };
        named
            .into_iter()
            .flatten()
            .chain(custom.iter().map(|(_, column)| *column))
    }

    /// `true` when the entity has no column at all: unrestricted, or scoped
    /// with every dimension, the type column included, `None` and no custom
    /// property.
    #[must_use]
    pub fn has_no_columns(&self) -> bool {
        self.type_col().is_none() && self.scope_columns().next().is_none()
    }
}

/// An entity that secure-orm can scope.
///
/// The whole contract is one constant describing the entity's [`EntityScope`].
/// Usually generated by `#[derive(Scopable)]` from `#[secure(...)]`
/// attributes; see the `secure` module docs.
///
/// # Example (manual implementation)
///
/// ```rust,ignore
/// use toolkit_db::secure::{Dimensions, EntityScope, ScopableEntity};
///
/// impl ScopableEntity for Entity {
///     const SCOPE: EntityScope<Column> = EntityScope::Scoped(Dimensions {
///         tenant: Some(Column::TenantId),
///         resource: Some(Column::Id),
///         owner: None,
///         type_col: None,
///         custom: &[("department_id", Column::DepartmentId)],
///     });
/// }
/// ```
///
/// # Example (derive)
///
/// ```rust,ignore
/// use toolkit_db::secure::Scopable;
///
/// #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
/// #[sea_orm(table_name = "users")]
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
/// ```
///
/// `#[secure(unrestricted)]` produces [`EntityScope::Unrestricted`].
pub trait ScopableEntity: EntityTrait {
    /// The entity's scope.
    const SCOPE: EntityScope<Self::Column>;
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "entity_traits_tests.rs"]
mod entity_traits_tests;
