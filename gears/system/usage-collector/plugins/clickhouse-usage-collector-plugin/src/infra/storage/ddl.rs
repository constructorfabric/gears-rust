//! A small `CREATE TABLE` builder for the statement shapes this plugin needs.
//!
//! Neither `sea-query` nor `sea-orm-migration` has a `ClickHouse` backend, and
//! their table builders have no `ENGINE`, `PARTITION BY`, `TTL`, `SETTINGS`,
//! `ON CLUSTER` or data-skipping indexes. This covers exactly those, nothing
//! more: the tables themselves are declared by the migration steps
//! (`migrator::mNNNN_*`) and rendered per topology by `migrator::table`.
//!
//! Identifiers are rendered verbatim, never quoted. Every table and cluster
//! name reaching here is a `'static` constant or validated config
//! (`ClickHouseClusterConfig::validate`); column types and key expressions are
//! `'static` constants. Only `COMMENT` strings are escaped.

/// One column: `name type [DEFAULT expr] [COMMENT '…']`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Column {
    name: &'static str,
    ty: &'static str,
    default: Option<&'static str>,
    comment: &'static str,
}

/// A data-skipping index: `INDEX name expr TYPE kind GRANULARITY n`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkipIndex {
    pub name: &'static str,
    pub expr: &'static str,
    pub kind: &'static str,
    pub granularity: u32,
}

/// Table engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Engine {
    /// `ReplacingMergeTree[(version)]`.
    ReplacingMergeTree { version: Option<&'static str> },
    /// `ReplicatedReplacingMergeTree[(['path', 'replica'][, version])]`.
    ///
    /// Without `keeper`, the server's `default_replica_path` /
    /// `default_replica_name` apply, which use the `{uuid}` macro: such a
    /// table can only be created `ON CLUSTER`, which hands every host the
    /// same table UUID and therefore the same Keeper path.
    ReplicatedReplacingMergeTree {
        keeper: Option<(String, String)>,
        version: Option<&'static str>,
    },
    /// `Distributed(cluster, currentDatabase(), table, sharding_key)`.
    Distributed {
        cluster: String,
        table: String,
        sharding_key: &'static str,
    },
}

impl Engine {
    /// The engine name as `system.tables.engine` reports it.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::ReplacingMergeTree { .. } => "ReplacingMergeTree",
            Self::ReplicatedReplacingMergeTree { .. } => "ReplicatedReplacingMergeTree",
            Self::Distributed { .. } => "Distributed",
        }
    }

    fn to_sql(&self) -> String {
        let args: Vec<String> = match self {
            Self::ReplacingMergeTree { version } => {
                version.iter().map(|v| (*v).to_owned()).collect()
            }
            Self::ReplicatedReplacingMergeTree { keeper, version } => keeper
                .iter()
                .flat_map(|(path, replica)| [quote(path), quote(replica)])
                .chain(version.iter().map(|v| (*v).to_owned()))
                .collect(),
            Self::Distributed {
                cluster,
                table,
                sharding_key,
            } => vec![
                cluster.clone(),
                "currentDatabase()".to_owned(),
                table.clone(),
                (*sharding_key).to_owned(),
            ],
        };
        if args.is_empty() {
            self.name().to_owned()
        } else {
            format!("{}({})", self.name(), args.join(", "))
        }
    }
}

/// What follows the table name: a column list, or `AS <table>`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Body {
    Columns {
        columns: Vec<Column>,
        indexes: Vec<SkipIndex>,
    },
    As(String),
}

/// A `CREATE TABLE` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateTable {
    name: String,
    if_not_exists: bool,
    on_cluster: Option<String>,
    body: Body,
    engine: Option<Engine>,
    partition_by: Option<&'static str>,
    order_by: Vec<&'static str>,
    ttl: Option<String>,
    settings: Vec<(&'static str, String)>,
}

impl CreateTable {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            if_not_exists: false,
            on_cluster: None,
            body: Body::Columns {
                columns: Vec::new(),
                indexes: Vec::new(),
            },
            engine: None,
            partition_by: None,
            order_by: Vec::new(),
            ttl: None,
            settings: Vec::new(),
        }
    }

    #[must_use]
    pub fn if_not_exists(mut self) -> Self {
        self.if_not_exists = true;
        self
    }

    /// `ON CLUSTER <cluster>`; `None` leaves the statement local.
    #[must_use]
    pub fn on_cluster(mut self, cluster: Option<&str>) -> Self {
        self.on_cluster = cluster.map(str::to_owned);
        self
    }

    /// Append a column `name ty COMMENT 'comment'` (no comment when empty).
    /// Replaces an `AS` body.
    #[must_use]
    pub fn column(self, name: &'static str, ty: &'static str, comment: &'static str) -> Self {
        self.push_column(Column {
            name,
            ty,
            default: None,
            comment,
        })
    }

    /// [`Self::column`] with a `DEFAULT` expression.
    #[must_use]
    pub fn column_with_default(
        self,
        name: &'static str,
        ty: &'static str,
        default: &'static str,
        comment: &'static str,
    ) -> Self {
        self.push_column(Column {
            name,
            ty,
            default: Some(default),
            comment,
        })
    }

    fn push_column(mut self, column: Column) -> Self {
        match &mut self.body {
            Body::Columns { columns, .. } => columns.push(column),
            Body::As(_) => {
                self.body = Body::Columns {
                    columns: vec![column],
                    indexes: Vec::new(),
                };
            }
        }
        self
    }

    /// Append data-skipping indexes; ignored on an `AS` body.
    #[must_use]
    pub fn indexes(mut self, idx: &[SkipIndex]) -> Self {
        if let Body::Columns { indexes, .. } = &mut self.body {
            indexes.extend_from_slice(idx);
        }
        self
    }

    /// `AS <table>`: copy `table`'s columns. Replaces a column list.
    #[must_use]
    pub fn copy_columns_of(mut self, table: impl Into<String>) -> Self {
        self.body = Body::As(table.into());
        self
    }

    #[must_use]
    pub fn engine(mut self, engine: Engine) -> Self {
        self.engine = Some(engine);
        self
    }

    #[must_use]
    pub fn partition_by(mut self, expr: &'static str) -> Self {
        self.partition_by = Some(expr);
        self
    }

    #[must_use]
    pub fn order_by(mut self, keys: &[&'static str]) -> Self {
        self.order_by = keys.to_vec();
        self
    }

    /// The `TTL` expression, without the keyword.
    #[must_use]
    pub fn ttl(mut self, expr: impl Into<String>) -> Self {
        self.ttl = Some(expr.into());
        self
    }

    /// Append one `SETTINGS` entry.
    #[must_use]
    pub fn setting(mut self, key: &'static str, value: &impl ToString) -> Self {
        self.settings.push((key, value.to_string()));
        self
    }

    /// Render the statement, without a trailing `;`.
    #[must_use]
    pub fn to_sql(&self) -> String {
        let mut sql = String::from("CREATE TABLE ");
        if self.if_not_exists {
            sql.push_str("IF NOT EXISTS ");
        }
        sql.push_str(&self.name);
        if let Some(cluster) = &self.on_cluster {
            sql.push_str(" ON CLUSTER ");
            sql.push_str(cluster);
        }
        match &self.body {
            Body::Columns { columns, indexes } => {
                let defs: Vec<String> = columns
                    .iter()
                    .map(column_sql)
                    .chain(indexes.iter().map(|i| {
                        format!(
                            "INDEX {} {} TYPE {} GRANULARITY {}",
                            i.name, i.expr, i.kind, i.granularity
                        )
                    }))
                    .collect();
                sql.push_str("\n(\n    ");
                sql.push_str(&defs.join(",\n    "));
                sql.push_str("\n)");
            }
            Body::As(table) => {
                sql.push_str("\nAS ");
                sql.push_str(table);
            }
        }
        if let Some(engine) = &self.engine {
            sql.push_str("\nENGINE = ");
            sql.push_str(&engine.to_sql());
        }
        if let Some(expr) = self.partition_by {
            sql.push_str("\nPARTITION BY ");
            sql.push_str(expr);
        }
        if !self.order_by.is_empty() {
            sql.push_str("\nORDER BY (");
            sql.push_str(&self.order_by.join(", "));
            sql.push(')');
        }
        if let Some(expr) = &self.ttl {
            sql.push_str("\nTTL ");
            sql.push_str(expr);
        }
        if !self.settings.is_empty() {
            let settings: Vec<String> = self
                .settings
                .iter()
                .map(|(k, v)| format!("{k} = {v}"))
                .collect();
            sql.push_str("\nSETTINGS ");
            sql.push_str(&settings.join(", "));
        }
        sql
    }
}

fn column_sql(c: &Column) -> String {
    let mut sql = format!("{} {}", c.name, c.ty);
    if let Some(expr) = c.default {
        sql.push_str(" DEFAULT ");
        sql.push_str(expr);
    }
    if !c.comment.is_empty() {
        sql.push_str(" COMMENT ");
        sql.push_str(&quote(c.comment));
    }
    sql
}

/// A single-quoted `ClickHouse` string literal.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "ddl_tests.rs"]
mod ddl_tests;
