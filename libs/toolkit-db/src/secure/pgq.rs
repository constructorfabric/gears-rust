//! Secure property-graph declarations.
//!
//! One Rust declaration is the source of **both** the `CREATE PROPERTY GRAPH`
//! DDL a migration executes and the labels a `MATCH` addresses
//! (`docs/arch/secure-orm/ADR/0002`, Policy 3). Nothing else keeps those two
//! sides in step, and the failure when they drift is quiet: a column left out of
//! the DDL's `PROPERTIES` list does not make the graph invalid, it just makes
//! that column invisible to the pattern language — so a scope column left out
//! makes the pattern unscopable while everything still parses.
//!
//! # What the declaration guarantees
//!
//! * **Every element exposes its scope columns as properties.** Derived from
//!   [`ScopableEntity::scope_columns`], not from a list the caller repeats, so
//!   the precondition for Policy 2 cannot be forgotten.
//! * **One label per element table.** Sharing a label across tables is legal
//!   SQL/PGQ and unsafe here: security is decided per entity, so a shared label
//!   would have several security mappings and the builder would have to pick
//!   one (Policy 1). The declaration refuses it.
//! * **Every element resolves at least one scope column.** An entity that
//!   resolves none compiles to `WHERE false` under any constrained scope — safe,
//!   and indistinguishable from a legitimate deny-all, which is why it is
//!   refused here rather than detected later (Policy 2).
//!
//! # Membership is a type-level fact
//!
//! [`VertexOf`] and [`EdgeOf`] are what make `label ↔ entity` compiler-checked
//! rather than a convention held in two files. A builder that accepts
//! `J: VertexOf<G>` cannot be handed an entity that is not part of `G`, and
//! cannot be handed an edge where a vertex belongs.

use std::collections::BTreeSet;
use std::marker::PhantomData;
use std::sync::Arc;

use sea_orm::sea_query::{Alias, IntoIden, SelectStatement, TableRef};
use sea_orm::{Condition, EntityTrait, FromQueryResult, QueryTrait, StatementBuilder};
use toolkit_sea_orm_pgq::{
    EdgeTable, ElementKey, EndpointRef, PropertyGraph as GraphDdl, VertexTable,
};

use crate::secure::cond::{ColumnAddress, SiblingSupport, build_scope_predicate};
use crate::secure::select::{Scoped, SecureSelect};
use crate::secure::{AccessScope, DBRunner, DBRunnerInternal, ScopableEntity, ScopeError};

/// A property graph the platform declares.
///
/// Implement it — or derive it — on a marker type, never on an entity: a graph
/// is a schema object spanning several tables.
pub trait PropertyGraph {
    /// Name of the graph object in the database.
    const GRAPH_NAME: &'static str;

    /// The declaration, from which both the DDL and the query AST are built.
    ///
    /// # Errors
    /// Returns [`ScopeError::Invalid`] when the declaration violates Policy 1
    /// or Policy 2.
    fn declaration() -> Result<GraphDeclaration, ScopeError>;
}

/// An entity that participates in `G` as a vertex.
pub trait VertexOf<G: PropertyGraph>: ScopableEntity + EntityTrait {
    /// Label the pattern language addresses this element by.
    const LABEL: &'static str;
}

/// An entity that participates in `G` as an edge.
pub trait EdgeOf<G: PropertyGraph>: ScopableEntity + EntityTrait {
    /// Label the pattern language addresses this element by.
    const LABEL: &'static str;
}

/// One endpoint of an edge: which columns point at which vertex table.
#[derive(Clone, Debug)]
pub struct Endpoint {
    /// Columns on the edge table.
    pub key: Vec<String>,
    /// Vertex table they reference.
    pub table: String,
    /// Columns on that vertex table.
    pub references: Vec<String>,
}

/// A graph declaration under construction.
///
/// Elements are added by entity type, so the label and the scope columns come
/// from the entity rather than from strings the caller repeats.
#[derive(Debug, Default)]
pub struct GraphDeclaration {
    name: String,
    vertices: Vec<VertexTable>,
    edges: Vec<EdgeTable>,
    labels: BTreeSet<String>,
}

impl GraphDeclaration {
    /// Start a declaration for `G`.
    #[must_use]
    pub fn new<G: PropertyGraph>() -> Self {
        Self {
            name: G::GRAPH_NAME.to_owned(),
            ..Self::default()
        }
    }

    /// Register `J` as a vertex, keyed on `key`.
    ///
    /// # Errors
    /// Returns [`ScopeError::Invalid`] when `J` resolves no scope column
    /// (Policy 2) or when its label is already taken (Policy 1).
    pub fn vertex<G, J>(mut self, key: &[&str]) -> Result<Self, ScopeError>
    where
        G: PropertyGraph,
        J: VertexOf<G>,
    {
        let element = self.element::<J>(J::LABEL, key)?;
        self.vertices.push(element);
        Ok(self)
    }

    /// Register `J` as an edge between two vertex tables.
    ///
    /// # Errors
    /// As [`Self::vertex`], plus an endpoint that names no columns.
    pub fn edge<G, J>(
        mut self,
        key: &[&str],
        source: Endpoint,
        destination: Endpoint,
    ) -> Result<Self, ScopeError>
    where
        G: PropertyGraph,
        J: EdgeOf<G>,
    {
        let element = self.element::<J>(J::LABEL, key)?;
        self.edges.push(EdgeTable {
            element,
            source: into_endpoint(source)?,
            destination: into_endpoint(destination)?,
        });
        Ok(self)
    }

    /// Build the element common to vertices and edges, enforcing both policies.
    fn element<J>(&mut self, label: &str, key: &[&str]) -> Result<VertexTable, ScopeError>
    where
        J: ScopableEntity + EntityTrait,
    {
        if key.is_empty() {
            return Err(ScopeError::Invalid(
                "a property-graph element needs a key; without one no edge can reference it",
            ));
        }

        // Policy 2, checked here rather than after compiling a condition: an
        // entity that resolves no scope column compiles to exactly the same
        // predicate under a real scope as a legitimate deny-all does, so by the
        // time there is a `Condition` the two are indistinguishable.
        let scope_columns = J::scope_columns();
        if scope_columns.is_empty() {
            return Err(ScopeError::Invalid(
                "a property-graph element must resolve at least one scope column; \
                 an element that resolves none would traverse as a silent deny-all",
            ));
        }

        // Policy 1: one label per element table. Legal SQL/PGQ, unsafe here.
        if !self.labels.insert(label.to_owned()) {
            return Err(ScopeError::Invalid(
                "two elements of one property graph share a label; security is decided \
                 per entity, so a shared label would have several security mappings",
            ));
        }

        // The properties list is the union of the key columns and every scope
        // column. The key is there so an endpoint reference can resolve; the
        // scope columns are there so a pattern can be scoped at all.
        let mut properties: Vec<String> = key.iter().map(|c| (*c).to_owned()).collect();
        for column in scope_columns {
            let name = sea_orm::IdenStatic::as_str(&column).to_owned();
            if !properties.contains(&name) {
                properties.push(name);
            }
        }

        Ok(VertexTable {
            table: J::default().table_name().to_owned(),
            key: ElementKey(key.iter().map(|c| (*c).to_owned()).collect()),
            label: label.to_owned(),
            properties,
        })
    }

    /// Render the `CREATE PROPERTY GRAPH` statement.
    ///
    /// # Errors
    /// Returns [`ScopeError::Invalid`] when the declaration cannot be rendered.
    pub fn create_statement(&self) -> Result<String, ScopeError> {
        self.ddl().create_statement().map_err(syntax_error)
    }

    /// Render the matching `DROP PROPERTY GRAPH IF EXISTS`.
    ///
    /// # Errors
    /// As [`Self::create_statement`].
    pub fn drop_statement(&self) -> Result<String, ScopeError> {
        self.ddl().drop_statement().map_err(syntax_error)
    }

    fn ddl(&self) -> GraphDdl {
        GraphDdl {
            name: self.name.clone(),
            vertices: self.vertices.clone(),
            edges: self.edges.clone(),
        }
    }

    /// Labels declared so far, for tests and diagnostics.
    pub fn labels(&self) -> impl Iterator<Item = &str> {
        self.labels.iter().map(String::as_str)
    }

    /// Properties exposed for `label`, if it is declared.
    #[must_use]
    pub fn properties_of(&self, label: &str) -> Option<&[String]> {
        self.vertices
            .iter()
            .chain(self.edges.iter().map(|e| &e.element))
            .find(|element| element.label == label)
            .map(|element| element.properties.as_slice())
    }
}

fn into_endpoint(endpoint: Endpoint) -> Result<EndpointRef, ScopeError> {
    if endpoint.key.is_empty() || endpoint.references.is_empty() {
        return Err(ScopeError::Invalid(
            "an edge endpoint needs both its own columns and the columns it references",
        ));
    }
    if endpoint.key.len() != endpoint.references.len() {
        return Err(ScopeError::Invalid(
            "an edge endpoint must reference as many columns as it carries",
        ));
    }
    Ok(EndpointRef {
        key: ElementKey(endpoint.key),
        table: endpoint.table,
        references: ElementKey(endpoint.references),
    })
}

/// A syntax-layer refusal, carried into this crate's error type.
///
/// The message is dropped rather than formatted in: `ScopeError::Invalid` holds
/// a `&'static str`, and the syntax layer's refusals are all about a declaration
/// this module already validated, so reaching one means a bug here rather than a
/// caller mistake.
fn syntax_error(_: toolkit_sea_orm_pgq::PgqError) -> ScopeError {
    ScopeError::Invalid("the property-graph declaration could not be rendered")
}

// ───────────────────────── the secure graph query ─────────────────────────

/// A graph query whose every element carries the caller's scope.
///
/// Reachable only from a scoped select, mirroring `with_ctes`: the scope the
/// elements inherit is the one the outer query already carries, so a
/// differently-scoped element cannot be constructed.
pub struct SecureGraphSelect<E: EntityTrait, G: PropertyGraph> {
    /// Outer query, scope `WHERE` already embedded by `scope_with`. Its table is
    /// the anchor a pattern may correlate against.
    outer: SelectStatement,
    /// Seeds every element predicate. This is what makes same-scope structural.
    scope: Arc<AccessScope>,
    pattern: Option<PathState>,
    columns: Vec<toolkit_sea_orm_pgq::ProjectedColumn>,
    /// Relations the element predicates correlate against, deduplicated by
    /// alias so one relation referenced by several elements is placed once.
    siblings: Vec<SiblingPlacement>,
    /// The first refusal raised inside a builder closure.
    ///
    /// The closures cannot return `Result`, so a refusal is carried here and
    /// surfaced at execution. What matters is that it is not silent: the query
    /// never runs with a missing predicate.
    error: Option<ScopeError>,
    graph_alias: &'static str,
    _marker: PhantomData<(E, G)>,
}

/// A correlated relation and the alias it is placed under.
#[derive(Clone)]
struct SiblingPlacement {
    alias: String,
    query: SelectStatement,
}

/// Pattern under construction, with the pending edge a hop needs.
#[derive(Default)]
struct PathState {
    head: Option<toolkit_sea_orm_pgq::Element>,
    hops: Vec<(
        toolkit_sea_orm_pgq::Element,
        toolkit_sea_orm_pgq::Direction,
        toolkit_sea_orm_pgq::Element,
    )>,
    pending_edge: Option<(toolkit_sea_orm_pgq::Element, toolkit_sea_orm_pgq::Direction)>,
}

impl<E> SecureSelect<E, Scoped>
where
    E: EntityTrait,
{
    /// Begin a graph query over `G`.
    ///
    /// Every pattern element registered on the result is scoped with **this**
    /// query's `AccessScope`. The outer query stays in the `FROM` as the anchor,
    /// which is what lets a pattern correlate against an already-scoped entity
    /// query — the "start from these rows and walk out one hop" shape.
    #[must_use]
    pub fn with_graph<G: PropertyGraph>(self) -> SecureGraphSelect<E, G> {
        let scope = self.scope_arc();
        SecureGraphSelect {
            outer: QueryTrait::into_query(self.into_inner()),
            scope,
            pattern: None,
            columns: Vec::new(),
            siblings: Vec::new(),
            error: None,
            graph_alias: "cf_graph",
            _marker: PhantomData,
        }
    }
}

/// Builds the pattern, attaching scope to every element as it is added.
///
/// Elements are addressed by entity type, never by label: security is decided
/// per entity, and one label may span several element tables, so a
/// label-addressed element would have several security mappings (Policy 1).
pub struct PathBuilder<G: PropertyGraph> {
    scope: Arc<AccessScope>,
    state: PathState,
    siblings: Vec<SiblingPlacement>,
    error: Option<ScopeError>,
    _marker: PhantomData<G>,
}

impl<G: PropertyGraph> PathBuilder<G> {
    /// Start the pattern at a vertex.
    #[must_use]
    pub fn vertex<J>(mut self, variable: &'static str) -> Self
    where
        J: VertexOf<G>,
        J::Column: sea_orm::ColumnTrait + Copy,
    {
        match self.scoped_element::<J>(variable, J::LABEL) {
            Ok(element) => {
                if self.state.head.is_some() {
                    self.fail(ScopeError::Invalid(
                        "a pattern has one head vertex; reach the others with edge_to/edge_from",
                    ));
                } else {
                    self.state.head = Some(element);
                }
            }
            Err(e) => self.fail(e),
        }
        self
    }

    /// Follow an edge away from the current vertex.
    #[must_use]
    pub fn edge_to<J>(mut self, variable: &'static str) -> Self
    where
        J: EdgeOf<G>,
        J::Column: sea_orm::ColumnTrait + Copy,
    {
        self.edge::<J>(variable, toolkit_sea_orm_pgq::Direction::Outgoing);
        self
    }

    /// Follow an edge into the current vertex.
    #[must_use]
    pub fn edge_from<J>(mut self, variable: &'static str) -> Self
    where
        J: EdgeOf<G>,
        J::Column: sea_orm::ColumnTrait + Copy,
    {
        self.edge::<J>(variable, toolkit_sea_orm_pgq::Direction::Incoming);
        self
    }

    /// Complete the hop at a vertex.
    #[must_use]
    pub fn to<J>(mut self, variable: &'static str) -> Self
    where
        J: VertexOf<G>,
        J::Column: sea_orm::ColumnTrait + Copy,
    {
        let Some((edge, direction)) = self.state.pending_edge.take() else {
            self.fail(ScopeError::Invalid(
                "to() completes a hop; call edge_to() or edge_from() first",
            ));
            return self;
        };
        match self.scoped_element::<J>(variable, J::LABEL) {
            Ok(target) => self.state.hops.push((edge, direction, target)),
            Err(e) => self.fail(e),
        }
        self
    }

    fn edge<J>(&mut self, variable: &'static str, direction: toolkit_sea_orm_pgq::Direction)
    where
        J: EdgeOf<G>,
        J::Column: sea_orm::ColumnTrait + Copy,
    {
        if self.state.head.is_none() {
            self.fail(ScopeError::Invalid(
                "a pattern starts at a vertex; call vertex() before an edge",
            ));
            return;
        }
        if self.state.pending_edge.is_some() {
            self.fail(ScopeError::Invalid(
                "two edges in a row; complete the hop with to() first",
            ));
            return;
        }
        match self.scoped_element::<J>(variable, J::LABEL) {
            Ok(element) => self.state.pending_edge = Some((element, direction)),
            Err(e) => self.fail(e),
        }
    }

    /// Build one element with its scope predicate attached.
    ///
    /// Policy 2 first: an entity that resolves no scope column is refused here,
    /// because after a condition exists it is indistinguishable from a
    /// legitimate deny-all.
    fn scoped_element<J>(
        &mut self,
        variable: &'static str,
        label: &'static str,
    ) -> Result<toolkit_sea_orm_pgq::Element, ScopeError>
    where
        J: ScopableEntity + EntityTrait,
        J::Column: sea_orm::ColumnTrait + Copy,
    {
        if J::scope_columns().is_empty() {
            return Err(ScopeError::Invalid(
                "a graph element must resolve at least one scope column; \
                 an element that resolves none would traverse as a silent deny-all",
            ));
        }

        let predicate = build_scope_predicate::<J>(
            &self.scope,
            ColumnAddress::GraphElement {
                var: variable,
                siblings: SiblingSupport::Allowed,
            },
        )?;
        let (condition, siblings) = predicate.into_parts();

        for sibling in siblings {
            // Deduplicated by alias: the same scope compiled for two elements
            // names the same relation, and placing it twice would turn the
            // correlation into a cross join and multiply rows.
            if !self.siblings.iter().any(|p| p.alias == sibling.alias) {
                self.siblings.push(SiblingPlacement {
                    alias: sibling.alias,
                    query: sibling.query,
                });
            }
        }

        Ok(toolkit_sea_orm_pgq::Element::new(variable, label).and_where(condition))
    }

    fn fail(&mut self, error: ScopeError) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }
}

impl<E, G> SecureGraphSelect<E, G>
where
    E: EntityTrait,
    G: PropertyGraph,
{
    /// Describe the pattern.
    ///
    /// Scope is attached to every element as it is added, so a predicate the
    /// closure adds afterwards narrows the element rather than replacing what
    /// the library put there.
    #[must_use]
    pub fn match_path(mut self, f: impl FnOnce(PathBuilder<G>) -> PathBuilder<G>) -> Self {
        let builder = f(PathBuilder {
            scope: Arc::clone(&self.scope),
            state: PathState::default(),
            siblings: Vec::new(),
            error: None,
            _marker: PhantomData,
        });

        if let Some(error) = builder.error {
            self.fail(error);
            return self;
        }
        if builder.state.pending_edge.is_some() {
            self.fail(ScopeError::Invalid(
                "the pattern ends on an edge; complete the hop with to()",
            ));
            return self;
        }

        for sibling in builder.siblings {
            if !self.siblings.iter().any(|p| p.alias == sibling.alias) {
                self.siblings.push(sibling);
            }
        }
        self.pattern = Some(builder.state);
        self
    }

    /// Project a graph property into the result, and select it in the outer
    /// query.
    ///
    /// Selecting it here as well is deliberate: a `COLUMNS` entry nothing selects
    /// transfers nothing, and a caller that has just named the properties it
    /// wants should not have to name them twice.
    #[must_use]
    pub fn column(
        mut self,
        variable: &'static str,
        property: &'static str,
        alias: &'static str,
    ) -> Self {
        self.columns.push(toolkit_sea_orm_pgq::ProjectedColumn::new(
            variable, property, alias,
        ));
        // First projected column replaces the anchor's `SELECT *`: a graph query
        // asked for graph columns.
        if self.columns.len() == 1 {
            self.outer.clear_selects();
        }
        self.outer.expr_as(
            sea_orm::sea_query::Expr::col((Alias::new(self.graph_alias), Alias::new(alias))),
            Alias::new(alias),
        );
        self
    }

    /// Narrow the outer query, on top of the scope the elements already carry.
    #[must_use]
    pub fn filter(mut self, filter: Condition) -> Self {
        self.outer.cond_where(filter);
        self
    }

    /// Cap the number of rows.
    #[must_use]
    pub fn limit(mut self, limit: u64) -> Self {
        self.outer.limit(limit);
        self
    }

    /// Deduplicate the outer result.
    #[must_use]
    pub fn distinct(mut self) -> Self {
        self.outer.distinct();
        self
    }

    fn fail(&mut self, error: ScopeError) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }

    /// Assemble the statement: the anchor, the correlated siblings, and the
    /// pattern, in one `FROM`.
    fn build(mut self) -> Result<SelectStatement, ScopeError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let Some(state) = self.pattern else {
            return Err(ScopeError::Invalid(
                "a graph query needs a pattern; call match_path()",
            ));
        };
        let Some(head) = state.head else {
            return Err(ScopeError::Invalid(
                "a graph query needs a head vertex; call vertex()",
            ));
        };

        let mut pattern = toolkit_sea_orm_pgq::GraphPattern::new(head);
        for (edge, direction, target) in state.hops {
            pattern = pattern.hop(edge, direction, target);
        }

        let mut table = toolkit_sea_orm_pgq::GraphTable::new(G::GRAPH_NAME, pattern);
        for column in self.columns {
            table = table.column(column);
        }

        // Siblings first, then the pattern: a comma join, which PostgreSQL
        // treats as an implicit lateral, so the pattern's correlated references
        // resolve. `LATERAL` itself is refused before `GRAPH_TABLE`.
        for sibling in self.siblings {
            self.outer.from(TableRef::SubQuery(
                Box::new(sibling.query),
                Alias::new(sibling.alias.as_str()).into_iden(),
            ));
        }
        self.outer.from(
            table
                .into_table_ref(self.graph_alias)
                .map_err(|_| ScopeError::Invalid("the graph pattern could not be rendered"))?,
        );

        Ok(self.outer)
    }

    /// Render the statement without executing it.
    ///
    /// # Errors
    /// Returns [`ScopeError::Invalid`] for a pattern that cannot be built.
    pub fn build_statement(
        self,
        backend: sea_orm::DbBackend,
    ) -> Result<sea_orm::Statement, ScopeError> {
        let query = self.build()?;
        Ok(StatementBuilder::build(&query, &backend))
    }

    /// Execute and deserialize into `T`.
    ///
    /// # Errors
    /// Returns [`ScopeError::Invalid`] for a pattern that cannot be built, or
    /// [`ScopeError::Db`] when the query fails.
    pub async fn all_as<T>(self, runner: &impl DBRunner) -> Result<Vec<T>, ScopeError>
    where
        T: FromQueryResult + Send + Sync,
    {
        let exec = DBRunnerInternal::as_seaorm(runner);
        let stmt = self.build_statement(exec.backend())?;
        Ok(match exec {
            crate::secure::SeaOrmRunner::Conn(db) => T::find_by_statement(stmt).all(db).await?,
            crate::secure::SeaOrmRunner::Tx(tx) => T::find_by_statement(stmt).all(tx).await?,
        })
    }
}
