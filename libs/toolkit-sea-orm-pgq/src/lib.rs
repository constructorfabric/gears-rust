//! SQL/PGQ (`GRAPH_TABLE`) syntax for `sea_query`.
//!
//! `sea_query` models no part of SQL/PGQ — verified against the pinned
//! `sea_query` 1.0.2, where the only occurrences of "graph" are in the `CYCLE`
//! clause documentation. This crate fills that gap: a typed AST for
//! `GRAPH_TABLE … MATCH … COLUMNS (…)` and for the `CREATE`/`DROP PROPERTY
//! GRAPH` DDL, plus a renderer that produces something a `sea_query`
//! statement can put in its `FROM`.
//!
//! # What this crate deliberately does not know
//!
//! Anything about security. There is no `AccessScope`, no `ScopableEntity` and
//! no database runner here, and there never should be: `toolkit-db` decides
//! *which* predicate is mandatory on an element, this crate only knows *how* to
//! write one. That split is what makes the syntax testable without a security
//! context, and it is the layering
//! `docs/arch/secure-orm/ADR/0002` fixes.
//!
//! Consequently: **using this crate directly from a gear is unsafe by
//! construction.** Nothing here embeds scope, so a pattern built straight from
//! this AST traverses whatever the graph contains. Gears go through
//! `toolkit-db`'s secure graph builder, which is the only thing that guarantees
//! every element carries a scope predicate.
//!
//! # Two properties the renderer is responsible for
//!
//! **Identifiers are escaped, never formatted.** Graph names, labels, element
//! variables and property names all become identifiers in the emitted SQL, and
//! all of them go through `sea_query`'s own quoting
//! ([`QuotedBuilder::prepare_iden`]) rather than through `format!`. A hostile
//! name therefore renders as exactly one quoted identifier.
//!
//! **Values are bound, never interpolated.** Element predicates arrive as
//! `sea_query` [`Condition`]s and are written into one shared
//! [`SqlWriterValues`], so placeholders are numbered continuously across the
//! whole construct and the values travel as parameters.
//!
//! # Directions are explicit
//!
//! There is no undirected element and no undirected shorthand, because
//! `(a)-[e]-(b)` plans as a parallel sequential scan of the edge table:
//! measured at 735 ms for one such element and 7967 ms for two, against ~1.5 ms
//! for the same result set written with arrows. An undirected hop is two
//! directed patterns.
//!
//! [`QuotedBuilder::prepare_iden`]: sea_orm::sea_query::QuotedBuilder::prepare_iden
//! [`SqlWriterValues`]: sea_orm::sea_query::SqlWriterValues
//! [`Condition`]: sea_orm::Condition

mod ddl;
mod render;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;

pub use ddl::{EdgeTable, ElementKey, EndpointRef, PropertyGraph, VertexTable};

use sea_orm::Condition;
use sea_orm::sea_query::TableRef;

/// Something the AST refuses to render.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PgqError {
    /// A pattern with no `COLUMNS` produces no output columns, so the enclosing
    /// query has nothing to select.
    #[error("a GRAPH_TABLE needs at least one projected column")]
    NoColumns,
    /// An identifier that cannot name anything.
    #[error("{what} must not be empty")]
    EmptyIdentifier {
        /// Which identifier was empty.
        what: &'static str,
    },
    /// Two elements of one pattern share a variable, so a predicate on that
    /// variable would be ambiguous.
    #[error("element variable `{0}` is used twice in one pattern")]
    DuplicateVariable(String),
}

/// Which way an edge is followed.
///
/// There is no undirected variant; see the crate documentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// `(a)-[e]->(b)` — from the preceding vertex to the following one.
    Outgoing,
    /// `(a)<-[e]-(b)` — into the preceding vertex from the following one.
    Incoming,
}

/// One pattern element: a vertex or an edge.
///
/// `filter` is written inside the element's own parentheses, which is what makes
/// a per-element predicate expressible at all — and, for `toolkit-db`, what
/// makes embedding scope per element possible.
#[derive(Clone, Debug)]
pub struct Element {
    variable: String,
    label: String,
    filter: Option<Condition>,
}

impl Element {
    /// An element bound to `variable`, matching `label`.
    pub fn new(variable: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            variable: variable.into(),
            label: label.into(),
            filter: None,
        }
    }

    /// Add a predicate inside this element's parentheses.
    ///
    /// Called more than once, the conditions are combined with `AND` — so a
    /// caller cannot
    /// replace a predicate that was already attached, only narrow it. That is
    /// what lets `toolkit-db` apply scope *after* a caller's own predicate and
    /// know it cannot be filtered back off.
    #[must_use]
    pub fn and_where(mut self, condition: Condition) -> Self {
        self.filter = Some(match self.filter.take() {
            Some(existing) => Condition::all().add(existing).add(condition),
            None => condition,
        });
        self
    }

    /// The variable this element is bound to.
    #[must_use]
    pub fn variable(&self) -> &str {
        &self.variable
    }

    /// The label this element matches.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The predicate attached so far, if any.
    #[must_use]
    pub fn filter(&self) -> Option<&Condition> {
        self.filter.as_ref()
    }
}

/// One hop: an edge and the vertex it reaches.
#[derive(Clone, Debug)]
struct Hop {
    edge: Element,
    direction: Direction,
    target: Element,
}

/// A fixed-depth path pattern.
///
/// `PostgreSQL` 19's initial SQL/PGQ implementation supports fixed-depth
/// patterns only, so a pattern spells out its hops. Variable-depth traversal is
/// a different tool (a recursive CTE or a closure table), not a longer pattern.
#[derive(Clone, Debug)]
pub struct GraphPattern {
    head: Element,
    hops: Vec<Hop>,
}

impl GraphPattern {
    /// Start a pattern at `head`.
    #[must_use]
    pub fn new(head: Element) -> Self {
        Self {
            head,
            hops: Vec::new(),
        }
    }

    /// Follow `edge` in `direction` to `target`.
    #[must_use]
    pub fn hop(mut self, edge: Element, direction: Direction, target: Element) -> Self {
        self.hops.push(Hop {
            edge,
            direction,
            target,
        });
        self
    }

    /// Every element in the pattern, head first, in written order.
    pub fn elements(&self) -> impl Iterator<Item = &Element> {
        std::iter::once(&self.head).chain(
            self.hops
                .iter()
                .flat_map(|hop| [&hop.edge, &hop.target].into_iter()),
        )
    }
}

/// One entry of the `COLUMNS` clause: a graph property projected as a column.
#[derive(Clone, Debug)]
pub struct ProjectedColumn {
    variable: String,
    property: String,
    alias: String,
}

impl ProjectedColumn {
    /// Project `variable.property` as `alias`.
    pub fn new(
        variable: impl Into<String>,
        property: impl Into<String>,
        alias: impl Into<String>,
    ) -> Self {
        Self {
            variable: variable.into(),
            property: property.into(),
            alias: alias.into(),
        }
    }
}

/// A whole `GRAPH_TABLE` construct.
#[derive(Clone, Debug)]
pub struct GraphTable {
    graph: String,
    pattern: GraphPattern,
    columns: Vec<ProjectedColumn>,
}

impl GraphTable {
    /// A construct over the property graph `graph`.
    pub fn new(graph: impl Into<String>, pattern: GraphPattern) -> Self {
        Self {
            graph: graph.into(),
            pattern,
            columns: Vec::new(),
        }
    }

    /// Project a graph property into the result.
    #[must_use]
    pub fn column(mut self, column: ProjectedColumn) -> Self {
        self.columns.push(column);
        self
    }

    /// Render as a `FROM` item aliased `alias`.
    ///
    /// # Errors
    /// Returns [`PgqError`] for a construct that cannot name anything: no
    /// projected columns, an empty identifier, or a variable used twice.
    pub fn into_table_ref(self, alias: &str) -> Result<TableRef, PgqError> {
        render::table_ref(&self, alias)
    }

    /// Render the construct's SQL and bound values, without executing it.
    ///
    /// Exposed for tests: assertions belong on rendered SQL, because a predicate
    /// that never reaches the database would satisfy a `Debug`-form assertion
    /// and still leak.
    ///
    /// # Errors
    /// As [`Self::into_table_ref`].
    pub fn render_for_test(&self) -> Result<(String, Vec<sea_orm::Value>), PgqError> {
        render::body(self)
    }
}
