//! Typed `OData` query builder
//!
//! This module provides a generic, reusable typed query builder for `OData` that produces
//! `ODataQuery` with correct filter hashing.
//!
//! # Design
//!
//! - **Schema trait**: Defines field enums and their string mappings (from `schema` module)
//! - **`FieldRef`**: Type-safe field references with schema and Rust type markers
//! - **Filter constructors**: Typed comparison and string operations returning AST expressions
//! - **`QueryBuilder`**: Fluent API for building queries with filter/order/select/limit
//!
//! # Example
//!
//! ```rust,ignore
//! use modkit_odata::{Schema, FieldRef, QueryBuilder, SortDir};
//!
//! #[derive(Copy, Clone, Eq, PartialEq)]
//! enum UserField {
//!     Id,
//!     Name,
//!     Email,
//! }
//!
//! struct UserSchema;
//!
//! impl Schema for UserSchema {
//!     type Field = UserField;
//!
//!     fn field_name(field: Self::Field) -> &'static str {
//!         match field {
//!             UserField::Id => "id",
//!             UserField::Name => "name",
//!             UserField::Email => "email",
//!         }
//!     }
//! }
//!
//! // Define typed field references
//! const ID: FieldRef<UserSchema, uuid::Uuid> = FieldRef::new(UserField::Id);
//! const NAME: FieldRef<UserSchema, String> = FieldRef::new(UserField::Name);
//!
//! // Build a query
//! let user_id = uuid::Uuid::nil();
//! let query = QueryBuilder::<UserSchema>::new()
//!     .filter(ID.eq(user_id).and(NAME.contains("john")))
//!     .order_by(NAME, SortDir::Asc)
//!     .page_size(50)
//!     .build();
//! ```

use crate::schema::{AsFieldKey, AsFieldName, FieldRef, Schema};
use crate::{
    ODataOrderBy, ODataQuery, OrderKey, SortDir, ast::Expr, pagination::short_filter_hash,
};
use std::marker::PhantomData;

/// Typed query builder for `OData` queries.
///
/// This builder provides a fluent API for constructing `ODataQuery` instances
/// with type-safe field references and automatic filter hashing.
///
/// # Example
///
/// ```rust,ignore
/// let query = QueryBuilder::<UserSchema>::new()
///     .filter(NAME.contains("john"))
///     .order_by(NAME, SortDir::Asc)
///     .select([NAME, EMAIL])
///     .page_size(50)
///     .build();
/// ```
pub struct QueryBuilder<S: Schema> {
    filter: Option<Expr>,
    order: Vec<OrderKey>,
    select: Option<Vec<S::Field>>,
    limit: Option<u64>,
    _phantom: PhantomData<S>,
}

impl<S: Schema> QueryBuilder<S> {
    /// Create a new empty query builder.
    #[must_use]
    pub fn new() -> Self {
        Self {
            filter: None,
            order: Vec::new(),
            select: None,
            limit: None,
            _phantom: PhantomData,
        }
    }

    /// Set the filter expression.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// builder.filter(ID.eq(user_id).and(NAME.contains("john")))
    /// ```
    #[must_use]
    pub fn filter(mut self, expr: Expr) -> Self {
        self.filter = Some(expr);
        self
    }

    /// Add an order-by clause.
    ///
    /// Can be called multiple times to add multiple sort keys.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// builder
    ///     .order_by(NAME, SortDir::Asc)
    ///     .order_by(ID, SortDir::Desc)
    /// ```
    #[must_use]
    pub fn order_by<F>(mut self, field: F, dir: SortDir) -> Self
    where
        F: AsFieldName,
    {
        self.order.push(OrderKey {
            field: field.as_field_name().to_owned(),
            dir,
        });
        self
    }

    /// Set the select fields (field projection).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// builder.select([NAME, EMAIL])
    /// builder.select(vec![NAME, EMAIL])
    ///
    /// // Backwards-compatible (still supported)
    /// builder.select(&[&ID, &NAME, &EMAIL])
    /// ```
    #[must_use]
    pub fn select<I>(mut self, fields: I) -> Self
    where
        I: IntoIterator,
        I::Item: AsFieldKey<S>,
    {
        let iter = fields.into_iter();
        let (lower, _) = iter.size_hint();
        let mut out = Vec::with_capacity(lower);
        for f in iter {
            out.push(f.as_field_key());
        }
        self.select = Some(out);
        self
    }

    /// Set the page size limit.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// builder.page_size(50)
    /// ```
    #[must_use]
    pub fn page_size(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Build the final `ODataQuery` with computed filter hash.
    ///
    /// The filter hash is computed using the stable hashing algorithm from
    /// `pagination::short_filter_hash`.
    pub fn build(self) -> ODataQuery {
        let filter_hash = short_filter_hash(self.filter.as_ref());

        let mut query = ODataQuery::new();

        if let Some(expr) = self.filter {
            query = query.with_filter(expr);
        }

        if !self.order.is_empty() {
            query = query.with_order(ODataOrderBy(self.order));
        }

        if let Some(limit) = self.limit {
            query = query.with_limit(limit);
        }

        if let Some(hash) = filter_hash {
            query = query.with_filter_hash(hash);
        }

        if let Some(fields) = self.select {
            let names: Vec<String> = fields
                .into_iter()
                .map(|k| FieldRef::<S, ()>::new(k).name().to_owned())
                .collect();
            query = query.with_select(names);
        }

        query
    }
}

impl<S: Schema> Default for QueryBuilder<S> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "builder_tests.rs"]
mod tests;
