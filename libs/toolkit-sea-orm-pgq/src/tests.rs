//! Tests for the syntax layer.
//!
//! Every assertion is on rendered SQL. The two that matter most are structural
//! rather than substring-based: a correctly escaped identifier still *contains*
//! the dangerous substring, so searching for it proves nothing either way.

use sea_orm::sea_query::{Alias, Expr, ExprTrait as _, PostgresQueryBuilder, Query};
use sea_orm::{Condition, Value};

use crate::{
    Direction, EdgeTable, Element, ElementKey, EndpointRef, GraphPattern, GraphTable, PgqError,
    ProjectedColumn, PropertyGraph, VertexTable,
};

fn simple() -> GraphTable {
    GraphTable::new(
        "kb",
        GraphPattern::new(Element::new("a", "node")).hop(
            Element::new("e", "edge"),
            Direction::Outgoing,
            Element::new("b", "node"),
        ),
    )
    .column(ProjectedColumn::new("b", "id", "neighbour"))
}

fn render(table: &GraphTable) -> String {
    table.render_for_test().expect("renders").0
}

#[test]
fn a_hop_renders_with_an_explicit_arrow() {
    let sql = render(&simple());
    assert_eq!(
        sql,
        r#""kb" MATCH ("a" IS "node")-["e" IS "edge"]->("b" IS "node") COLUMNS ("b"."id" AS "neighbour")"#
    );
}

/// An incoming hop is a distinct pattern, not a flag on the same one — and it
/// must not degrade into the undirected shorthand, which is orders of magnitude
/// slower for the same rows.
#[test]
fn an_incoming_hop_reverses_the_arrow() {
    let table = GraphTable::new(
        "kb",
        GraphPattern::new(Element::new("a", "node")).hop(
            Element::new("e", "edge"),
            Direction::Incoming,
            Element::new("b", "node"),
        ),
    )
    .column(ProjectedColumn::new("b", "id", "n"));
    let sql = render(&table);
    assert!(
        sql.contains(r#"<-["e" IS "edge"]-("b" IS "node")"#),
        "{sql}"
    );
    // The undirected form is `-[e]-`; neither arrowless shape may appear.
    assert!(!sql.contains(r#"]-("b"#) || sql.contains(r#"]-("b" IS "node")"#));
}

/// A predicate lives inside the element's own parentheses. This is what makes
/// per-element scoping expressible at all, so its placement is load-bearing
/// rather than cosmetic.
#[test]
fn a_predicate_goes_inside_its_own_element() {
    let table = GraphTable::new(
        "kb",
        GraphPattern::new(
            Element::new("a", "node")
                .and_where(Condition::all().add(Expr::val(7).eq(Expr::val(7)))),
        ),
    )
    .column(ProjectedColumn::new("a", "id", "n"));
    let sql = render(&table);
    assert!(
        sql.contains(r#"("a" IS "node" WHERE "#),
        "the predicate must be inside the element: {sql}"
    );
}

/// Attaching a second predicate narrows; it cannot replace the first. That is
/// what lets a security layer apply its predicate after a caller's and know the
/// caller cannot filter it back off.
#[test]
fn a_second_predicate_narrows_rather_than_replaces() {
    let element = Element::new("a", "node")
        .and_where(Condition::all().add(Expr::val(1).eq(Expr::val(1))))
        .and_where(Condition::all().add(Expr::val(2).eq(Expr::val(2))));
    let table = GraphTable::new("kb", GraphPattern::new(element))
        .column(ProjectedColumn::new("a", "id", "n"));
    let sql = render(&table);
    assert_eq!(sql.matches(" AND ").count(), 1, "both must survive: {sql}");
}

/// Values are bound, never written into the SQL. A pattern that interpolated
/// them would be an injection surface and would also defeat statement caching.
#[test]
fn values_are_bound_not_interpolated() {
    let table = GraphTable::new(
        "kb",
        GraphPattern::new(
            Element::new("a", "node")
                .and_where(Condition::all().add(Expr::col(Alias::new("id")).eq("o'brien"))),
        ),
    )
    .column(ProjectedColumn::new("a", "id", "n"));
    let (sql, values) = table.render_for_test().expect("renders");
    assert!(
        !sql.contains("o'brien"),
        "the value must not appear in the SQL: {sql}"
    );
    assert!(sql.contains("$1"), "expected a placeholder: {sql}");
    assert_eq!(values, vec![Value::from("o'brien")]);
}

/// Placeholders are numbered continuously across elements. Each condition
/// rendered on its own would restart at `$1`, and splicing those fragments would
/// bind the wrong value to the wrong element.
#[test]
fn placeholders_are_numbered_across_the_whole_pattern() {
    let table = GraphTable::new(
        "kb",
        GraphPattern::new(
            Element::new("a", "node")
                .and_where(Condition::all().add(Expr::col(Alias::new("id")).eq(1))),
        )
        .hop(
            Element::new("e", "edge")
                .and_where(Condition::all().add(Expr::col(Alias::new("kind")).eq(2))),
            Direction::Outgoing,
            Element::new("b", "node")
                .and_where(Condition::all().add(Expr::col(Alias::new("id")).eq(3))),
        ),
    )
    .column(ProjectedColumn::new("b", "id", "n"));
    let (sql, values) = table.render_for_test().expect("renders");
    assert!(
        sql.contains("$1") && sql.contains("$2") && sql.contains("$3"),
        "{sql}"
    );
    assert_eq!(values.len(), 3);
}

/// Everything outside a quoted identifier, with the identifiers removed.
///
/// Substring searches over the whole statement prove nothing about escaping: a
/// correctly escaped identifier still *contains* the dangerous text, which is
/// exactly what makes a naive assertion pass on both the safe and the unsafe
/// rendering. So the check is structural — strip the quoted regions, honouring
/// `""` as an escaped quote, and assert on what is left, which is the only part
/// `PostgreSQL` reads as syntax.
fn outside_identifiers(sql: &str) -> Result<String, &'static str> {
    let mut out = String::new();
    let mut chars = sql.chars().peekable();
    let mut inside = false;
    while let Some(c) = chars.next() {
        match (c, inside) {
            ('"', false) => inside = true,
            ('"', true) => {
                if chars.peek() == Some(&'"') {
                    // A doubled quote is one literal character of the name, so
                    // the identifier does not end here.
                    chars.next();
                } else {
                    inside = false;
                }
            }
            (other, false) => out.push(other),
            (_, true) => {}
        }
    }
    if inside {
        // An unclosed identifier is itself a break-out: the rest of the
        // statement has been swallowed into a name.
        return Err("an identifier was never closed");
    }
    Ok(out)
}

/// A hostile name must render as exactly one identifier — it must not be able
/// to close its own quoting and become syntax.
#[test]
fn a_hostile_identifier_renders_as_one_escaped_identifier() {
    let hostile = r#"a" IS "node") COLUMNS ("x"."y" AS "z"); DROP TABLE t; --"#;
    let table = GraphTable::new("kb", GraphPattern::new(Element::new(hostile, "node")))
        .column(ProjectedColumn::new(hostile, "id", "n"));
    let sql = render(&table);

    let syntax = outside_identifiers(&sql).expect("identifiers must be balanced");
    assert_eq!(
        syntax.matches(" MATCH ").count(),
        1,
        "the payload became a second MATCH: {syntax}"
    );
    assert_eq!(
        syntax.matches(" COLUMNS (").count(),
        1,
        "the payload became a second COLUMNS: {syntax}"
    );
    assert!(
        !syntax.contains("DROP"),
        "the payload escaped its identifier: {syntax}"
    );
    assert!(
        !syntax.contains("--"),
        "the payload introduced a comment: {syntax}"
    );
}

/// The scanner the test above relies on has to be right, or that test is
/// decorative. Two renderings a `format!`-built renderer would have produced
/// must both be visible to it.
#[test]
fn the_escaping_check_can_actually_fail() {
    // Break-out that leaves the quoting balanced: the payload closes the
    // element and opens a whole second COLUMNS clause. Written as the
    // counterfactual a `format!`-built renderer would have produced, so the
    // example cannot drift from what it is meant to represent.
    let payload = r#"a" IS "node") COLUMNS ("x"."y" AS "z"#;
    let balanced = format!(r#""kb" MATCH ("{payload}" IS "node") COLUMNS ("a"."id" AS "n")"#);
    let syntax = outside_identifiers(&balanced).expect("balanced");
    assert!(
        syntax.matches(" COLUMNS (").count() > 1,
        "the scanner must see the second clause: {syntax}"
    );

    // And the same for the payload the positive test actually uses, whose odd
    // quote count leaves everything after it swallowed into a name. The scanner
    // reports that rather than quietly returning a truncated view.
    let with_comment = r#"a" IS "node") COLUMNS ("x"."y" AS "z"); DROP TABLE t; --"#;
    let unbalanced =
        format!(r#""kb" MATCH ("{with_comment}" IS "node") COLUMNS ("a"."id" AS "n")"#);
    assert!(outside_identifiers(&unbalanced).is_err());
}

/// The construct name is a keyword, so it must NOT be quoted — quoting it makes
/// the statement a syntax error rather than a slow query, which is the kind of
/// mistake that is easy to make once identifiers are being escaped everywhere.
#[test]
fn the_construct_name_is_not_quoted() {
    let table_ref = simple().into_table_ref("g").expect("renders");
    let sql = Query::select()
        .expr(Expr::val(1))
        .from(table_ref)
        .to_string(PostgresQueryBuilder);
    assert!(sql.contains("GRAPH_TABLE("), "{sql}");
    assert!(!sql.contains(r#""GRAPH_TABLE""#), "{sql}");
}

/// The construct is usable where a table is, which is the whole point of it
/// being a table function.
#[test]
fn it_can_be_placed_in_a_from_clause() {
    let table_ref = simple().into_table_ref("g").expect("renders");
    let sql = Query::select()
        .expr(Expr::val(1))
        .from(table_ref)
        .to_string(PostgresQueryBuilder);
    assert!(sql.contains(r#"AS "g""#), "{sql}");
}

#[test]
fn a_construct_without_columns_is_refused() {
    let table = GraphTable::new("kb", GraphPattern::new(Element::new("a", "node")));
    assert_eq!(table.render_for_test().unwrap_err(), PgqError::NoColumns);
}

#[test]
fn an_empty_identifier_is_refused() {
    let table = GraphTable::new("", GraphPattern::new(Element::new("a", "node")))
        .column(ProjectedColumn::new("a", "id", "n"));
    assert!(matches!(
        table.render_for_test().unwrap_err(),
        PgqError::EmptyIdentifier { .. }
    ));
}

/// One variable naming two elements would make a predicate on it ambiguous —
/// and would make "scope is attached per element" false, since two elements
/// would share a qualifier.
#[test]
fn a_repeated_variable_is_refused() {
    let table = GraphTable::new(
        "kb",
        GraphPattern::new(Element::new("a", "node")).hop(
            Element::new("e", "edge"),
            Direction::Outgoing,
            Element::new("a", "node"),
        ),
    )
    .column(ProjectedColumn::new("a", "id", "n"));
    assert_eq!(
        table.render_for_test().unwrap_err(),
        PgqError::DuplicateVariable("a".to_owned())
    );
}

// ─────────────────────────────── DDL ───────────────────────────────

fn graph_ddl() -> PropertyGraph {
    let node = VertexTable {
        table: "graph_node".to_owned(),
        key: ElementKey(vec!["tenant_id".to_owned(), "id".to_owned()]),
        label: "node".to_owned(),
        properties: vec!["tenant_id".to_owned(), "id".to_owned()],
    };
    PropertyGraph {
        name: "kb".to_owned(),
        vertices: vec![node],
        edges: vec![EdgeTable {
            element: VertexTable {
                table: "graph_edge".to_owned(),
                key: ElementKey(vec!["tenant_id".to_owned(), "id".to_owned()]),
                label: "edge".to_owned(),
                properties: vec!["tenant_id".to_owned(), "id".to_owned()],
            },
            source: EndpointRef {
                key: ElementKey(vec!["tenant_id".to_owned(), "src".to_owned()]),
                table: "graph_node".to_owned(),
                references: ElementKey(vec!["tenant_id".to_owned(), "id".to_owned()]),
            },
            destination: EndpointRef {
                key: ElementKey(vec!["tenant_id".to_owned(), "dst".to_owned()]),
                table: "graph_node".to_owned(),
                references: ElementKey(vec!["tenant_id".to_owned(), "id".to_owned()]),
            },
        }],
    }
}

/// An edge's clauses must come in grammar order: `KEY`, then `SOURCE`, then
/// `DESTINATION`, then `LABEL`. Asserted as an ordering rather than by
/// containment, because a statement with every clause present in the wrong order
/// still contains all of them — the first version of this test passed while the
/// generated DDL was a syntax error `PostgreSQL` rejected at `LABEL`.
#[test]
fn an_edge_declares_its_clauses_in_grammar_order() {
    let sql = graph_ddl().create_statement().expect("renders");
    let edges = sql.split("EDGE TABLES").nth(1).expect("has edge tables");
    let key = edges.find(" KEY (").expect("KEY");
    let source = edges.find("SOURCE KEY").expect("SOURCE");
    let destination = edges.find("DESTINATION KEY").expect("DESTINATION");
    let label = edges.find("LABEL").expect("LABEL");
    assert!(
        key < source && source < destination && destination < label,
        "clauses out of grammar order in: {edges}"
    );
}

/// The endpoint keys carry the tenant column, which is what makes an edge
/// structurally unable to join a vertex of another tenant — before any scope
/// predicate is applied.
#[test]
fn the_ddl_declares_composite_endpoint_keys() {
    let sql = graph_ddl().create_statement().expect("renders");
    assert!(
        sql.contains(
            r#"SOURCE KEY ("tenant_id", "src") REFERENCES "graph_node" ("tenant_id", "id")"#
        ),
        "{sql}"
    );
    assert!(sql.contains("CREATE PROPERTY GRAPH \"kb\""), "{sql}");
    assert!(
        sql.contains(r#"LABEL "node" PROPERTIES ("tenant_id", "id")"#),
        "{sql}"
    );
}

/// A graph with no exposed properties would be unfilterable, so it is refused
/// rather than created — the failure it prevents is silent.
#[test]
fn an_element_without_properties_is_refused() {
    let mut graph = graph_ddl();
    graph.vertices[0].properties.clear();
    assert!(matches!(
        graph.create_statement().unwrap_err(),
        PgqError::EmptyIdentifier { .. }
    ));
}

#[test]
fn the_drop_statement_is_idempotent() {
    assert_eq!(
        graph_ddl().drop_statement().expect("renders"),
        r#"DROP PROPERTY GRAPH IF EXISTS "kb""#
    );
}
