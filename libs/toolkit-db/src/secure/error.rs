use uuid::Uuid;

/// Errors that can occur during scoped query execution.
///
/// `#[non_exhaustive]`: this is the error enum every gear's repository layer
/// matches on, and the ORM grows variants a gear has no specific answer for
/// (the graph-query refusals below are constructible only under the `pgq`
/// feature). A downstream `match` keeps one wildcard arm for those instead of
/// gaining a dead arm per new variant — the cost is that a future variant a
/// gear *should* handle specifically lands in the wildcard rather than failing
/// its build, which is the standard trade for a library error enum.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum ScopeError {
    /// Database error occurred during query execution.
    #[error("database error: {0}")]
    Db(#[from] sea_orm::DbErr),

    /// Invalid scope configuration.
    #[error("invalid scope: {0}")]
    Invalid(&'static str),

    /// Tenant isolation violation: `tenant_id` is not included in the current scope.
    #[error("access denied: tenant_id not present in security scope ({tenant_id})")]
    TenantNotInScope { tenant_id: Uuid },

    /// Operation denied - entity not accessible in current security scope.
    #[error("access denied: {0}")]
    Denied(&'static str),

    /// A graph pattern element on which no constraint of the live scope
    /// resolves. Compiling it would produce a deny-all traversal that reads as
    /// missing data, so it is refused by name instead
    /// (`docs/arch/secure-orm/ADR/0002`, Policy 2).
    #[error(
        "invalid scope: no constraint of the scope resolves on graph element \
         `{element}` (property `{property}` does not resolve on its entity)"
    )]
    UnresolvedScopeProperty {
        /// The pattern variable of the refused element.
        element: &'static str,
        /// A property the scope addresses that the element's entity cannot map.
        property: String,
    },

    /// The SQL/PGQ syntax layer refused to render a graph declaration or
    /// pattern: no projected columns, a duplicate pattern variable, an empty
    /// identifier, an endpoint whose key and referenced columns differ in
    /// arity. The message is the syntax layer's own.
    ///
    /// The payload is this crate's, not the syntax crate's error type. The
    /// variant exists in every build — a feature-gated variant would change
    /// the enum's shape under feature unification — while the syntax crate is
    /// linked only under `pgq`: a gear on `SQLite` must not carry a `PostgreSQL` 19
    /// dependency to name this arm (`docs/arch/secure-orm/ADR/0002`, "Backend
    /// gating").
    #[error("graph syntax error: {0}")]
    GraphSyntax(String),
}

impl ScopeError {
    /// Returns `true` if this error wraps a unique-constraint violation.
    #[must_use]
    pub fn is_unique_violation(&self) -> bool {
        match self {
            Self::Db(db_err) => crate::db_error::is_unique_violation(db_err),
            _ => false,
        }
    }

    /// Returns `true` if this error wraps a foreign-key violation.
    #[must_use]
    pub fn is_foreign_key_violation(&self) -> bool {
        match self {
            Self::Db(db_err) => crate::db_error::is_foreign_key_violation(db_err),
            _ => false,
        }
    }
}
