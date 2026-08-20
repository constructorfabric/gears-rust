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

use sea_orm::EntityTrait;
use toolkit_sea_orm_pgq::{
    EdgeTable, ElementKey, EndpointRef, PropertyGraph as GraphDdl, VertexTable,
};

use crate::secure::{ScopableEntity, ScopeError};

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
