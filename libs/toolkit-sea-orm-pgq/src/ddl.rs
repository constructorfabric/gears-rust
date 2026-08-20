//! `CREATE` / `DROP PROPERTY GRAPH` DDL.
//!
//! A property graph is a schema object, created by a migration. This module
//! renders the statement; deciding *what* to declare is
//! `toolkit-db`'s job, driven by a single Rust declaration so that the labels a
//! `MATCH` uses and the tables behind them cannot drift apart
//! (`docs/arch/secure-orm/ADR/0002`, Policy 3).
//!
//! The one thing worth stating loudly: **a column absent from an element's
//! `PROPERTIES` list is invisible to `MATCH`.** Not an error — just silently
//! unfilterable, which for a scope column means the pattern cannot be scoped at
//! all. That failure mode is why generation exists.

use sea_orm::sea_query::{Alias, IntoIden, PostgresQueryBuilder, QuotedBuilder};

use crate::PgqError;

fn write_ident(out: &mut String, name: &str) {
    let mut buffer = String::new();
    PostgresQueryBuilder.prepare_iden(&Alias::new(name).into_iden(), &mut buffer);
    out.push_str(&buffer);
}

fn write_ident_list(out: &mut String, names: &[String]) {
    for (index, name) in names.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        write_ident(out, name);
    }
}

fn require_named(value: &str, what: &'static str) -> Result<(), PgqError> {
    if value.trim().is_empty() {
        return Err(PgqError::EmptyIdentifier { what });
    }
    Ok(())
}

/// The columns that identify an element row.
///
/// Composite on purpose in every graph this platform declares: a key carrying
/// the tenant means an edge structurally cannot join a vertex of another tenant,
/// before any scope predicate is applied.
#[derive(Clone, Debug)]
pub struct ElementKey(pub Vec<String>);

/// One vertex table and the label it is declared under.
#[derive(Clone, Debug)]
pub struct VertexTable {
    /// Table backing the element.
    pub table: String,
    /// Key columns.
    pub key: ElementKey,
    /// Label the pattern language addresses it by. One label per table: sharing
    /// a label across tables would give one label several security mappings.
    pub label: String,
    /// Columns exposed as properties. A scope column missing here cannot be
    /// filtered on inside a pattern.
    pub properties: Vec<String>,
}

/// Which vertex an edge endpoint points at.
#[derive(Clone, Debug)]
pub struct EndpointRef {
    /// Columns on the edge table.
    pub key: ElementKey,
    /// Vertex table referenced.
    pub table: String,
    /// Columns on that vertex table.
    pub references: ElementKey,
}

/// One edge table, its label and its two endpoints.
#[derive(Clone, Debug)]
pub struct EdgeTable {
    /// The element itself.
    pub element: VertexTable,
    /// Source endpoint.
    pub source: EndpointRef,
    /// Destination endpoint.
    pub destination: EndpointRef,
}

/// A whole property-graph declaration.
#[derive(Clone, Debug)]
pub struct PropertyGraph {
    /// Name of the graph object.
    pub name: String,
    /// Vertex tables.
    pub vertices: Vec<VertexTable>,
    /// Edge tables.
    pub edges: Vec<EdgeTable>,
}

impl PropertyGraph {
    /// Render `CREATE PROPERTY GRAPH`.
    ///
    /// # Errors
    /// Returns [`PgqError`] when an identifier is empty or the graph declares no
    /// vertex tables — a graph with only edges cannot resolve an endpoint.
    pub fn create_statement(&self) -> Result<String, PgqError> {
        require_named(&self.name, "a property graph name")?;
        if self.vertices.is_empty() {
            return Err(PgqError::EmptyIdentifier {
                what: "a property graph's vertex table list",
            });
        }

        let mut sql = String::from("CREATE PROPERTY GRAPH ");
        write_ident(&mut sql, &self.name);

        sql.push_str("\n  VERTEX TABLES (\n");
        for (index, vertex) in self.vertices.iter().enumerate() {
            if index > 0 {
                sql.push_str(",\n");
            }
            write_element(&mut sql, vertex)?;
        }
        sql.push_str("\n  )");

        if !self.edges.is_empty() {
            sql.push_str("\n  EDGE TABLES (\n");
            for (index, edge) in self.edges.iter().enumerate() {
                if index > 0 {
                    sql.push_str(",\n");
                }
                write_element(&mut sql, &edge.element)?;
                sql.push_str("\n      SOURCE KEY (");
                write_endpoint(&mut sql, &edge.source)?;
                sql.push_str("\n      DESTINATION KEY (");
                write_endpoint(&mut sql, &edge.destination)?;
            }
            sql.push_str("\n  )");
        }

        Ok(sql)
    }

    /// Render `DROP PROPERTY GRAPH IF EXISTS`.
    ///
    /// # Errors
    /// Returns [`PgqError::EmptyIdentifier`] for an unnamed graph.
    pub fn drop_statement(&self) -> Result<String, PgqError> {
        require_named(&self.name, "a property graph name")?;
        let mut sql = String::from("DROP PROPERTY GRAPH IF EXISTS ");
        write_ident(&mut sql, &self.name);
        Ok(sql)
    }
}

fn write_element(sql: &mut String, element: &VertexTable) -> Result<(), PgqError> {
    require_named(&element.table, "an element table name")?;
    require_named(&element.label, "an element label")?;
    if element.key.0.is_empty() {
        return Err(PgqError::EmptyIdentifier {
            what: "an element key",
        });
    }
    if element.properties.is_empty() {
        return Err(PgqError::EmptyIdentifier {
            what: "an element's property list",
        });
    }

    sql.push_str("    ");
    write_ident(sql, &element.table);
    sql.push_str(" KEY (");
    write_ident_list(sql, &element.key.0);
    sql.push_str(")\n      LABEL ");
    write_ident(sql, &element.label);
    sql.push_str(" PROPERTIES (");
    write_ident_list(sql, &element.properties);
    sql.push(')');
    Ok(())
}

fn write_endpoint(sql: &mut String, endpoint: &EndpointRef) -> Result<(), PgqError> {
    require_named(&endpoint.table, "an endpoint table name")?;
    if endpoint.key.0.is_empty() || endpoint.references.0.is_empty() {
        return Err(PgqError::EmptyIdentifier {
            what: "an endpoint key",
        });
    }
    write_ident_list(sql, &endpoint.key.0);
    sql.push_str(") REFERENCES ");
    write_ident(sql, &endpoint.table);
    sql.push_str(" (");
    write_ident_list(sql, &endpoint.references.0);
    sql.push(')');
    Ok(())
}
