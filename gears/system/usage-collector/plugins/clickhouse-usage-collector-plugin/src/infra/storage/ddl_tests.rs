use super::{CreateTable, Engine, SkipIndex};

#[test]
fn renders_every_clause_in_order() {
    let sql = CreateTable::new("t")
        .if_not_exists()
        .on_cluster(Some("c"))
        .column("id", "UUID", "Row id")
        .column_with_default("at", "DateTime64(3)", "now64(3)", "")
        .indexes(&[SkipIndex {
            name: "idx_id",
            expr: "id",
            kind: "bloom_filter",
            granularity: 1,
        }])
        .engine(Engine::ReplacingMergeTree { version: Some("v") })
        .partition_by("toYYYYMM(at)")
        .order_by(&["id", "at"])
        .ttl("at + INTERVAL 60 SECOND DELETE")
        .setting("a", &1)
        .setting("b", &"x")
        .to_sql();
    assert_eq!(
        sql,
        "CREATE TABLE IF NOT EXISTS t ON CLUSTER c\n\
         (\n    \
         id UUID COMMENT 'Row id',\n    \
         at DateTime64(3) DEFAULT now64(3),\n    \
         INDEX idx_id id TYPE bloom_filter GRANULARITY 1\n\
         )\n\
         ENGINE = ReplacingMergeTree(v)\n\
         PARTITION BY toYYYYMM(at)\n\
         ORDER BY (id, at)\n\
         TTL at + INTERVAL 60 SECOND DELETE\n\
         SETTINGS a = 1, b = x"
    );
}

#[test]
fn omits_absent_clauses() {
    let sql = CreateTable::new("t")
        .on_cluster(None)
        .column("id", "UUID", "Row id")
        .engine(Engine::ReplacingMergeTree { version: None })
        .order_by(&["id"])
        .to_sql();
    assert_eq!(
        sql,
        "CREATE TABLE t\n(\n    id UUID COMMENT 'Row id'\n)\nENGINE = ReplacingMergeTree\nORDER BY (id)"
    );
}

#[test]
fn distributed_table_copies_columns_with_as() {
    let sql = CreateTable::new("t")
        .if_not_exists()
        .on_cluster(Some("c"))
        .copy_columns_of("t_local")
        .engine(Engine::Distributed {
            cluster: "c".to_owned(),
            table: "t_local".to_owned(),
            sharding_key: "cityHash64(id)",
        })
        .to_sql();
    assert_eq!(
        sql,
        "CREATE TABLE IF NOT EXISTS t ON CLUSTER c\nAS t_local\n\
         ENGINE = Distributed(c, currentDatabase(), t_local, cityHash64(id))"
    );
}

#[test]
fn replicated_engine_with_and_without_keeper_args() {
    let defaults = Engine::ReplicatedReplacingMergeTree {
        keeper: None,
        version: Some("version"),
    };
    assert_eq!(defaults.to_sql(), "ReplicatedReplacingMergeTree(version)");

    let explicit = Engine::ReplicatedReplacingMergeTree {
        keeper: Some(("/p/{shard}".to_owned(), "{replica}".to_owned())),
        version: None,
    };
    assert_eq!(
        explicit.to_sql(),
        "ReplicatedReplacingMergeTree('/p/{shard}', '{replica}')"
    );
    assert_eq!(explicit.name(), "ReplicatedReplacingMergeTree");
}

#[test]
fn comments_are_escaped() {
    let sql = CreateTable::new("t")
        .column("x", "String", "it's a \\ path; ok")
        .to_sql();
    assert!(sql.contains(r"COMMENT 'it\'s a \\ path; ok'"), "{sql}");
}

#[test]
fn copy_columns_of_and_column_replace_each_other() {
    let sql = CreateTable::new("t")
        .column("id", "UUID", "Row id")
        .copy_columns_of("u")
        .to_sql();
    assert_eq!(sql, "CREATE TABLE t\nAS u");
    let sql = CreateTable::new("t")
        .copy_columns_of("u")
        .column("id", "UUID", "Row id")
        .to_sql();
    assert!(sql.contains("id UUID") && !sql.contains("AS u"), "{sql}");
}
