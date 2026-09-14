//! A canonical, diffable rendering of the schema a migration chain produces.
//!
//! # Why this exists
//!
//! The chain is to be re-authored — 91 migrations that patch one another collapsed into one
//! migration per table. The property that makes that safe is not "the new files look right", it
//! is **the new chain produces the same schema as the old one**. This module is the instrument
//! that decides it, so that the claim is a check rather than a reading.
//!
//! # What "canonical" has to mean here
//!
//! Two chains that produce the same schema must produce the same dump, or the instrument
//! manufactures work. Three sources of false difference are dealt with explicitly:
//!
//! **Ordering.** `sqlite_master` returns rows in creation order, which is exactly what the
//! re-authoring changes. Everything is therefore sorted by kind and then by name, and a table's
//! columns are sorted by name rather than by `cid` — a re-authored `CREATE TABLE` writes the
//! columns in one pass and will not reproduce the order that thirty `ALTER TABLE ADD COLUMN`
//! statements left behind. Column *order* is not a property this schema depends on; if that ever
//! stops being true, this is the line to revisit.
//!
//! **Formatting.** The DDL text stored in `sqlite_master` is the text that was executed, so a
//! re-indented `CREATE TRIGGER` differs textually while meaning the same thing. Runs of
//! whitespace are collapsed — but only *outside* single-quoted literals, because a trigger's
//! `RAISE(ABORT, 'two  spaces')` carries its message to the caller and collapsing that would
//! silently rewrite an error string. See [`normalise_sql`].
//!
//! **Build history.** A table's stanza does **not** carry its `CREATE TABLE` text. `SQLite` stores
//! the statement it executed, and rewrites it on `ALTER TABLE ... RENAME TO` — so a table that
//! reached its shape through a rebuild carries the rebuild's column order and the quotes the
//! rename added, neither of which is schema. Requiring a re-authored migration to reproduce that
//! would make every one of them a transcription of whichever historical rebuild happened to be
//! last, which is the opposite of the point. Tables are therefore rendered structurally: columns
//! from `PRAGMA table_info`, named constraints parsed out of the DDL and sorted, foreign keys
//! from `PRAGMA foreign_key_list`. Indexes and triggers keep their DDL, because there the text
//! *is* the meaning and no rename rewrites it.
//!
//! **Generated names.** `sqlite_autoindex_<table>_<n>` is invented by `SQLite` for a `UNIQUE`
//! declared inside a table, and its number moves when the constraint order moves. They are
//! excluded rather than normalised, and what that costs is exact: tables are rendered
//! structurally rather than by DDL, so an excluded auto-index is covered only by the `UNIQUE`
//! parsed out of the DDL beside it. That holds for every one in this chain because each `UNIQUE`
//! is named — nothing enforces it, and an unnamed one would be dropped by both halves at once.
//! An index the chain declares by name is `origin = 'c'` and is kept.
//!
//! # What it deliberately does not do
//!
//! It does not compare. A dump is a value; deciding whether two of them may differ is the
//! caller's, because the two engines and the two questions ("is this the same schema" versus "is
//! this the schema we meant") want different answers. It also does not read row data: this is a
//! schema oracle, and a chain that migrates data is checked by its own cases.
//!
//! # One stated limitation
//!
//! The caller applies the chain by driving `MigrationTrait::up` through a `SchemaManager`, not
//! through `run_migrations_for_testing`, because the platform runner hands back a `Db` whose
//! connection is `pub(crate)` and a test in another crate cannot reach it. The neighbouring
//! trigger and index censuses in `sqlite_migrations` take the same route for the same reason.
//!
//! What that costs: the platform runner wraps each `up` in a transaction and writes a ledger row,
//! and this path does neither. For a chain whose every `up` succeeds the resulting schema is the
//! same, which is the property being measured -- but if a dump taken this way ever disagreed with
//! a database the runner built, that disagreement is a finding about the runner and not noise.

#![allow(dead_code)]

use std::fmt::Write as _;

use bss_pricing::infra::storage::migrations::Migrator;
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};

/// Collapse runs of whitespace outside single-quoted literals, and trim the result.
///
/// SQL is whitespace-insensitive between tokens and **not** inside a literal, so this is the
/// widest normalisation that cannot change meaning. Doubled quotes (`''`) are the `SQLite`
/// escape for
/// a quote inside a literal and are handled by the same state machine that handles the literal:
/// the closing quote flips the state, and the next character re-opens it.
///
/// Whitespace immediately inside a parenthesis is dropped as well: whether `CHECK (` is
/// followed by a newline is the author's formatting, not the rule.
///
/// Identifiers quoted with `"` are left alone by the state machine because whitespace inside a
/// quoted identifier is already impossible to introduce accidentally by re-indenting, and
/// treating them as literals would defeat the collapse across the common
/// `"col" ,  "col2"` shape.
#[must_use]
pub fn normalise_sql(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut in_literal = false;
    let mut pending_space = false;

    for ch in sql.chars() {
        if in_literal {
            out.push(ch);
            if ch == '\'' {
                in_literal = false;
            }
            continue;
        }
        if ch == '\'' {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            in_literal = true;
            out.push(ch);
            continue;
        }
        if ch.is_whitespace() {
            pending_space = true;
            continue;
        }
        // A space next to a parenthesis carries nothing, and whether one is there depends only on
        // whether the author put a newline after `CHECK (`. Suppressing it is what lets a
        // re-authored constraint compare equal to the historical rebuild that first spelled it.
        let after_open = out.ends_with('(');
        if pending_space && !out.is_empty() && !after_open && ch != ')' {
            out.push(' ');
        }
        pending_space = false;
        out.push(ch);
    }
    out
}

/// Named constraints parsed out of a table's stored DDL, normalised and sorted.
///
/// `SQLite` has no catalog for these -- a `CHECK` exists only as text inside the `CREATE TABLE` --
/// so they are read back out of it. Parsing rather than keeping the whole statement is what makes
/// the dump insensitive to column order and to the quoting a `RENAME` introduces, while keeping
/// the part that carries meaning.
fn constraints_in(ddl: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = ddl;
    while let Some(at) = rest.find("CONSTRAINT ") {
        rest = &rest[at + "CONSTRAINT ".len()..];
        let Some(name_end) = rest.find(char::is_whitespace) else {
            break;
        };
        let name = rest[..name_end].to_owned();
        let body = &rest[name_end..];
        // Take to the end of the constraint's own parenthesised body, so a `CHECK` containing
        // parentheses is not cut short by the first `)` that closes an inner group.
        let Some(open) = body.find('(') else {
            continue;
        };
        let mut depth = 0i32;
        let mut end = open;
        for (i, ch) in body[open..].char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        let kind = body[..open].trim();
        out.push(normalise_sql(&format!(
            "CONSTRAINT {name} {kind} {}",
            &body[open..end]
        )));
    }
    out.sort();
    out
}

/// Foreign keys as `PRAGMA foreign_key_list` reports them -- structural, so an unnamed one is
/// still seen.
async fn foreign_keys_of(conn: &DatabaseConnection, table: &str) -> Vec<String> {
    let sql = format!("PRAGMA foreign_key_list('{table}')");
    let rows = conn
        .query_all_raw(Statement::from_string(DatabaseBackend::Sqlite, sql))
        .await
        .expect("read foreign_key_list");
    let mut out: Vec<String> = rows
        .iter()
        .map(|row| {
            let target: String = row.try_get("", "table").expect("the referenced table");
            let from: String = row.try_get("", "from").expect("the referencing column");
            let to: Option<String> = row.try_get("", "to").ok();
            format!("  FK {from} -> {target}.{}", to.as_deref().unwrap_or("?"))
        })
        .collect();
    out.sort();
    out
}

/// One row of `sqlite_master`, reduced to what the dump renders.
struct MasterRow {
    kind: String,
    name: String,
    table: String,
    sql: Option<String>,
}

/// One column of a table, as `PRAGMA table_info` reports it.
struct ColumnRow {
    name: String,
    kind: String,
    not_null: bool,
    default: Option<String>,
    primary_key: i32,
}

async fn master_rows(conn: &DatabaseConnection) -> Vec<MasterRow> {
    // `name NOT LIKE 'sqlite_%'` drops the engine's own bookkeeping tables and the auto-indexes
    // described in the module doc. `seaql_migrations` is the ledger the runner writes, not
    // schema this chain declares, so it is dropped too -- otherwise the dump would change with
    // the number of migrations, which is the one thing the re-authoring is meant to change.
    let sql = "SELECT type AS k, name AS n, tbl_name AS t, sql AS s \
               FROM sqlite_master \
               WHERE name NOT LIKE 'sqlite_%' AND name <> 'seaql_migrations' \
               ORDER BY type, name";
    let rows = conn
        .query_all_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            sql.to_owned(),
        ))
        .await
        .expect("query sqlite_master");

    let mut out: Vec<MasterRow> = rows
        .iter()
        .map(|row| MasterRow {
            kind: row.try_get("", "k").expect("the object kind"),
            name: row.try_get("", "n").expect("the object name"),
            table: row.try_get("", "t").expect("the owning table"),
            sql: row.try_get("", "s").ok(),
        })
        .collect();
    out.sort_by(|a, b| (&a.kind, &a.name).cmp(&(&b.kind, &b.name)));
    out
}

async fn columns_of(conn: &DatabaseConnection, table: &str) -> Vec<ColumnRow> {
    // The table name comes from `sqlite_master`, not from a caller, so it cannot carry a quote;
    // `PRAGMA` takes no bind parameters, which is why it is formatted in at all.
    let sql = format!("PRAGMA table_info('{table}')");
    let rows = conn
        .query_all_raw(Statement::from_string(DatabaseBackend::Sqlite, sql))
        .await
        .expect("read table_info");

    let mut out: Vec<ColumnRow> = rows
        .iter()
        .map(|row| ColumnRow {
            name: row.try_get("", "name").expect("the column name"),
            kind: row.try_get("", "type").expect("the declared type"),
            not_null: row.try_get::<i32>("", "notnull").expect("the null flag") == 1,
            default: row.try_get("", "dflt_value").ok(),
            primary_key: row.try_get("", "pk").expect("the key position"),
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Render the schema of an already-migrated `SQLite` connection.
///
/// The shape is one object per stanza, kinds in `sqlite_master` order (`index`, `table`,
/// `trigger`, `view`), names sorted inside each kind, so a diff of two dumps opens at the first
/// object that actually differs and names it on the stanza header.
pub async fn sqlite_dump(conn: &DatabaseConnection) -> String {
    let mut out = String::new();
    for row in master_rows(conn).await {
        writeln!(out, "{} {}", row.kind.to_uppercase(), row.name)
            .expect("a write to a String cannot fail");
        if row.kind == "table" {
            for col in columns_of(conn, &row.name).await {
                let null = if col.not_null { "NOT NULL" } else { "NULL" };
                let default = col.default.as_deref().unwrap_or("-");
                writeln!(
                    out,
                    "  COLUMN {} {} {} DEFAULT {} PK {}",
                    col.name, col.kind, null, default, col.primary_key
                )
                .expect("a write to a String cannot fail");
            }
        } else if row.table != row.name {
            writeln!(out, "  ON {}", row.table).expect("a write to a String cannot fail");
        }
        // A `NULL` sql is an object SQLite invented rather than one the chain declared. The
        // module doc argues why those are excluded; reaching here means one slipped past the
        // `sqlite_%` filter, so the dump says so rather than rendering an empty line that would
        // read as "no DDL".
        match row.sql.as_deref() {
            Some(sql) if row.kind == "table" => {
                for constraint in constraints_in(sql) {
                    writeln!(out, "  {constraint}").expect("a write to a String cannot fail");
                }
                for fk in foreign_keys_of(conn, &row.name).await {
                    writeln!(out, "{fk}").expect("a write to a String cannot fail");
                }
            }
            Some(sql) => {
                writeln!(out, "  DDL {}", normalise_sql(sql))
                    .expect("a write to a String cannot fail");
            }
            None => out.push_str("  DDL <generated by the engine, not declared by the chain>\n"),
        }
    }
    out
}

/// Every table name the dump carries, in order -- the coverage check's operand.
#[must_use]
pub fn tables_in(dump: &str) -> Vec<String> {
    dump.lines()
        .filter_map(|line| line.strip_prefix("TABLE "))
        .map(str::to_owned)
        .collect()
}

/// The chain in the order the platform runner applies it -- by migration **name**.
///
/// `Migrator::migrations()` returns declaration order, which is the registry's order and not
/// necessarily the applied one. Sorting by name here is what makes a dump taken through
/// `SchemaManager` comparable with a database the runner built.
#[must_use]
pub fn name_ordered_chain() -> Vec<Box<dyn MigrationTrait>> {
    let mut chain = Migrator::migrations();
    chain.sort_by(|a, b| a.name().cmp(b.name()));
    chain
}

/// Apply the whole chain to a bare connection and return its dump.
pub async fn migrate_and_dump_sqlite(conn: &DatabaseConnection) -> String {
    let manager = SchemaManager::new(conn);
    for migration in &name_ordered_chain() {
        migration
            .up(&manager)
            .await
            .unwrap_or_else(|e| panic!("up {} must succeed: {e}", migration.name()));
    }
    sqlite_dump(conn).await
}

/// Run a catalog query whose single column is aliased `v`.
///
/// A local copy of `pg_support::catalog_strings` rather than a call into it: this module is
/// included by test crates that do not all declare `mod pg_support`, and a shared module that
/// only compiles inside some of its includers is a trap for the next person.
async fn catalog_lines(conn: &DatabaseConnection, sql: &str) -> Vec<String> {
    conn.query_all_raw(Statement::from_string(
        DatabaseBackend::Postgres,
        sql.to_owned(),
    ))
    .await
    .expect("run the catalog query")
    .iter()
    .map(|row| row.try_get::<String>("", "v").expect("read the value"))
    .collect()
}

/// Render the schema of an already-migrated Postgres database.
///
/// # Why this is the stronger half of the oracle
///
/// Postgres re-renders a constraint or an index from its parsed form —
/// `pg_get_constraintdef` and `pg_get_indexdef` return the server's own canonical spelling, not
/// the text that was submitted. So a re-authored migration that writes the same rule differently
/// compares equal here **without** any normalisation of mine, and a difference that survives is a
/// difference the server itself sees. The `SQLite` half has to work from stored DDL text and
/// cannot offer that.
///
/// Function bodies are the exception: `pg_get_functiondef` returns the body verbatim, newlines
/// and all, so those go through [`normalise_sql`] like the `SQLite` side.
///
/// # Both schemas, deliberately
///
/// The chain is applied under a `public,bss` search path and creates its objects in `bss`. The
/// dump reads **both** schemas and prints the schema on every line, so an object that lands in
/// the wrong one is a visible difference rather than an invisible absence. That is not
/// hypothetical: `toolkit-db`'s migration runner creates its history table unqualified, which
/// under a `bss`-first path puts it somewhere the next boot does not look.
pub async fn postgres_dump(conn: &DatabaseConnection) -> String {
    // The ledger is bookkeeping the runner writes, not schema this chain declares, and its row
    // count changes with the length of the chain -- which is the one thing the re-authoring is
    // meant to change. Excluded on both engines for the same reason.
    //
    // Matched by pattern, not by name: `toolkit-db` names it `toolkit_migrations_<gear>_<hash>`,
    // so the literal `seaql_migrations` this first tried excluded nothing. The name is stable for
    // a given gear -- which is why the determinism case passed while the filter was wrong, and
    // why determinism alone is never enough to trust a dump.
    const NOT_THE_LEDGER: &str =
        "c.relname NOT LIKE 'toolkit\\_migrations%' AND c.relname <> 'seaql_migrations'";

    let columns = format!(
        "SELECT 'COLUMN ' || n.nspname || '.' || c.relname || ' ' || a.attname || ' ' \
         || format_type(a.atttypid, a.atttypmod) \
         || CASE WHEN a.attnotnull THEN ' NOT NULL' ELSE ' NULL' END \
         || ' DEFAULT ' || COALESCE(pg_get_expr(d.adbin, d.adrelid), '-') AS v \
         FROM pg_attribute a \
         JOIN pg_class c ON c.oid = a.attrelid \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
         WHERE n.nspname IN ('bss', 'public') AND c.relkind = 'r' \
         AND a.attnum > 0 AND NOT a.attisdropped AND {NOT_THE_LEDGER} \
         ORDER BY 1"
    );
    let constraints = format!(
        "SELECT 'CONSTRAINT ' || n.nspname || '.' || c.relname || ' ' || con.conname || ' ' \
         || pg_get_constraintdef(con.oid) AS v \
         FROM pg_constraint con \
         JOIN pg_class c ON c.oid = con.conrelid \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname IN ('bss', 'public') AND {NOT_THE_LEDGER} \
         ORDER BY 1"
    );
    let indexes = "SELECT 'INDEX ' || schemaname || ' ' || indexname || ' ' || indexdef AS v \
                   FROM pg_indexes \
                   WHERE schemaname IN ('bss', 'public') \
                   AND tablename NOT LIKE 'toolkit\\_migrations%' \
                   AND tablename <> 'seaql_migrations' \
                   ORDER BY 1";
    let triggers = format!(
        "SELECT 'TRIGGER ' || n.nspname || '.' || c.relname || ' ' || t.tgname || ' ' \
         || pg_get_triggerdef(t.oid) AS v \
         FROM pg_trigger t \
         JOIN pg_class c ON c.oid = t.tgrelid \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname IN ('bss', 'public') AND NOT t.tgisinternal AND {NOT_THE_LEDGER} \
         ORDER BY 1"
    );
    // Functions an extension owns are excluded and the **extension** is dumped instead.
    // `CREATE EXTENSION btree_gist` (excl_pricing_price_window_no_overlap) installs dozens of C functions into
    // `public`; listing their signatures would bury the schema under the extension's contents,
    // while dropping them silently would hide the fact that an extension is required at all.
    // `pg_depend` with `deptype = 'e'` is what distinguishes the two.
    let functions = "SELECT 'FUNCTION ' || n.nspname || ' ' || p.proname || ' ' \
                     || pg_get_functiondef(p.oid) AS v \
                     FROM pg_proc p \
                     JOIN pg_namespace n ON n.oid = p.pronamespace \
                     WHERE n.nspname IN ('bss', 'public') \
                     AND NOT EXISTS ( \
                       SELECT 1 FROM pg_depend d \
                       WHERE d.objid = p.oid AND d.classid = 'pg_proc'::regclass \
                       AND d.deptype = 'e') \
                     ORDER BY 1";
    let extensions = "SELECT 'EXTENSION ' || extname AS v FROM pg_extension \
                      WHERE extname <> 'plpgsql' ORDER BY 1";

    let mut out = String::new();
    for sql in [
        columns.as_str(),
        constraints.as_str(),
        indexes,
        triggers.as_str(),
        functions,
        extensions,
    ] {
        for line in catalog_lines(conn, sql).await {
            // Every kind goes through the same normalisation even though only the function
            // bodies need it: a rule applied to some lines and not others is a rule the next
            // reader has to re-derive from which query produced which line.
            writeln!(out, "{}", normalise_sql(&line)).expect("a write to a String cannot fail");
        }
    }
    out
}
