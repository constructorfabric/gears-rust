use super::*;
#[tokio::test]
async fn every_forward_prefix_installs_and_preserves_old_audit_and_order_rows() -> anyhow::Result<()>
{
    let pg = Pg::bare().await?;
    let total = migrations::all().len();
    for end in 1..=total {
        let mut prefix = migrations::all();
        prefix.truncate(end);
        toolkit_db::migration_runner::run_migrations_for_testing(&pg.db, prefix).await?;
        if end == 3 {
            create(&pg.db, 1).await?;
            let mut row = super::constraints::audit(101);
            row.hash_version = 1;
            super::constraints::write(&pg, row).await?;
            let mut row = super::constraints::audit(102);
            row.hash_version = 2;
            super::constraints::write(&pg, row).await?;
        }
        if end >= 3 {
            assert_eq!(
                pg.scalar("SELECT count(*) AS n FROM bss_orders__order")
                    .await?,
                1
            );
            assert_eq!(
                pg.scalar("SELECT count(*) AS n FROM bss_orders__transition_audit")
                    .await?,
                2
            );
        }
    }
    for m in migrations::all().into_iter().skip(2) {
        assert!(
            m.down(&sea_orm_migration::SchemaManager::new(&pg.raw))
                .await
                .is_err()
        );
    }
    Ok(())
}
#[tokio::test]
async fn manifest_matches_every_actual_column_type_nullability_key_and_guard() -> anyhow::Result<()>
{
    let pg = Pg::new().await?;
    let inv: serde_json::Value =
        serde_json::from_str(include_str!("../migrations/schema-inventory.json"))?;
    for table in inv["tables"].as_array().unwrap() {
        let name = table["physical"].as_str().unwrap();
        let rows=pg.raw.query_all_raw(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT attname,format_type(atttypid,atttypmod) AS typ,attnotnull FROM pg_attribute WHERE attrelid=$1::regclass AND attnum>0 AND NOT attisdropped",[name.into()])).await?;
        assert_eq!(
            rows.len(),
            table["columns"].as_array().unwrap().len(),
            "{name}"
        );
        for col in table["columns"].as_array().unwrap() {
            let r = rows
                .iter()
                .find(|r| {
                    r.try_get::<String>("", "attname").unwrap() == col["name"].as_str().unwrap()
                })
                .unwrap();
            let wanted = match col["type"].as_str().unwrap() {
                "char(3)" => "character(3)",
                "timestamptz" => "timestamp with time zone",
                t => t,
            };
            assert_eq!(
                r.try_get::<String>("", "typ")?,
                wanted,
                "{name}.{}",
                col["name"]
            );
            assert_eq!(
                r.try_get::<bool>("", "attnotnull")?,
                !col["nullable"].as_bool().unwrap()
            );
        }
        for name in table["constraints"].as_object().unwrap().keys() {
            assert_eq!(
                pg.scalar(&format!(
                    "SELECT count(*) AS n FROM pg_constraint WHERE conname='{name}'"
                ))
                .await?,
                1,
                "{name}"
            );
        }
        for name in table["indexes"].as_object().unwrap().keys() {
            assert_eq!(
                pg.scalar(&format!(
                    "SELECT count(*) AS n FROM pg_indexes WHERE indexname='{name}'"
                ))
                .await?,
                1,
                "{name}"
            );
        }
        assert_eq!(pg.scalar(&format!("SELECT count(*) AS n FROM pg_trigger WHERE tgrelid='{name}'::regclass AND NOT tgisinternal")).await?, i64::try_from(table["triggers"].as_array().unwrap().len())?);
        for trigger in table["triggers"].as_array().unwrap() {
            let words: Vec<_> = trigger.as_str().unwrap().split_whitespace().collect();
            let trigger_name = words[if words[1] == "CONSTRAINT" { 3 } else { 2 }];
            assert_eq!(pg.scalar(&format!("SELECT count(*) AS n FROM pg_trigger WHERE tgrelid='{name}'::regclass AND tgname='{trigger_name}' AND tgenabled='O'")).await?, 1);
        }
        for grant in table["grants"].as_array().unwrap() {
            let grant = grant.as_str().unwrap();
            let (privileges, rest) = grant
                .strip_prefix("GRANT ")
                .unwrap()
                .split_once(" ON ")
                .unwrap();
            let roles = rest.split_once(" TO ").unwrap().1.trim_end_matches(';');
            for role in roles.split(',').map(str::trim) {
                if let Some(columns) = privileges
                    .strip_prefix("UPDATE (")
                    .and_then(|s| s.strip_suffix(')'))
                {
                    for column in columns.split(',') {
                        assert_eq!(pg.scalar(&format!("SELECT has_column_privilege('{role}','{name}','{column}','UPDATE')::integer::bigint AS n")).await?, 1);
                    }
                } else {
                    for privilege in privileges.split(',').map(str::trim) {
                        assert_eq!(pg.scalar(&format!("SELECT has_table_privilege('{role}','{name}','{privilege}')::integer::bigint AS n")).await?, 1);
                    }
                }
            }
        }
    }
    Ok(())
}
fn squash(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && *c != '(' && *c != ')')
        .collect()
}
/// Name-only checks cannot catch a drifted column list, a lost partial predicate or an extra
/// privilege. Compare definitions and require the actual sets to equal the manifest exactly.
#[tokio::test]
async fn manifest_definitions_and_privileges_are_exact_with_no_undeclared_objects()
-> anyhow::Result<()> {
    use std::collections::BTreeSet;
    let pg = Pg::new().await?;
    let inv: serde_json::Value =
        serde_json::from_str(include_str!("../migrations/schema-inventory.json"))?;
    let rows = |sql: &str, name: &str| {
        pg.raw.query_all_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            [name.into()],
        ))
    };
    for table in inv["tables"].as_array().unwrap() {
        let name = table["physical"].as_str().unwrap();
        let constraints = table["constraints"].as_object().unwrap();
        let actual = rows("SELECT conname::text AS n,contype::text AS t,condeferrable AS d,condeferred AS i,pg_get_constraintdef(oid) AS def FROM pg_constraint WHERE conrelid=$1::regclass AND contype IN ('p','u','f','c')", name).await?;
        let names: BTreeSet<String> = actual.iter().map(|r| r.try_get("", "n").unwrap()).collect();
        assert_eq!(
            names,
            constraints.keys().cloned().collect::<BTreeSet<_>>(),
            "{name} constraints"
        );
        for r in &actual {
            let conname: String = r.try_get("", "n")?;
            let expected = constraints[&conname].as_str().unwrap();
            let kind = match r.try_get::<String>("", "t")?.as_str() {
                "p" => "PRIMARY KEY",
                "u" => "UNIQUE",
                "f" => "FOREIGN KEY",
                _ => "CHECK",
            };
            assert!(expected.starts_with(kind), "{conname}: {expected}");
            let deferred = expected.contains("DEFERRABLE INITIALLY DEFERRED");
            assert_eq!(r.try_get::<bool>("", "d")?, deferred, "{conname}");
            assert_eq!(r.try_get::<bool>("", "i")?, deferred, "{conname}");
            // PostgreSQL normalizes CHECK expressions; key columns and references are exact.
            if kind != "CHECK" {
                assert_eq!(
                    squash(&r.try_get::<String>("", "def")?),
                    squash(expected),
                    "{conname}"
                );
            }
        }
        let indexes = table["indexes"].as_object().unwrap();
        let backing: BTreeSet<String> = constraints
            .iter()
            .filter(|(_, d)| {
                let d = d.as_str().unwrap();
                d.starts_with("PRIMARY KEY") || d.starts_with("UNIQUE")
            })
            .map(|(n, _)| n.clone())
            .collect();
        let actual = rows("SELECT c.relname::text AS n,x.indisunique AS u,x.indnullsnotdistinct AS nd,pg_get_expr(x.indpred,x.indrelid) AS p,array_to_string(ARRAY(SELECT pg_get_indexdef(x.indexrelid,k::int,true) FROM generate_series(1,x.indnkeyatts) k ORDER BY k),',') AS cols FROM pg_index x JOIN pg_class c ON c.oid=x.indexrelid WHERE x.indrelid=$1::regclass", name).await?;
        let names: BTreeSet<String> = actual.iter().map(|r| r.try_get("", "n").unwrap()).collect();
        assert_eq!(
            names,
            indexes
                .keys()
                .cloned()
                .chain(backing)
                .collect::<BTreeSet<_>>(),
            "{name} indexes"
        );
        for r in &actual {
            let index: String = r.try_get("", "n")?;
            let Some(expected) = indexes.get(&index).and_then(|d| d.as_str()) else {
                continue;
            };
            let (keys, predicate) = expected
                .split_once(" WHERE ")
                .map_or((expected, None), |(k, p)| (k, Some(p)));
            assert_eq!(
                r.try_get::<bool>("", "u")?,
                keys.starts_with("UNIQUE"),
                "{index}"
            );
            assert_eq!(
                r.try_get::<bool>("", "nd")?,
                keys.contains("NULLS NOT DISTINCT"),
                "{index}"
            );
            let keys = keys
                .trim_start_matches("UNIQUE ")
                .trim_end_matches(" NULLS NOT DISTINCT");
            assert_eq!(
                squash(&r.try_get::<String>("", "cols")?),
                squash(keys),
                "{index}"
            );
            // PostgreSQL rewrites predicates (casts, ANY(ARRAY[..])); require the same partial
            // shape and every declared column/literal token.
            let actual_predicate: Option<String> = r.try_get("", "p")?;
            assert_eq!(actual_predicate.is_some(), predicate.is_some(), "{index}");
            if let (Some(actual), Some(declared)) = (actual_predicate, predicate) {
                for token in declared
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '\''))
                    // Uppercase SQL keywords (IN, IS, NOT) may be rewritten; names/literals not.
                    .filter(|t| !t.is_empty() && t.chars().any(|c| !c.is_ascii_uppercase()))
                {
                    assert!(actual.contains(token), "{index}: {token} in {actual}");
                }
            }
        }
        let mut declared = BTreeSet::new();
        for grant in table["grants"].as_array().unwrap() {
            let grant = grant.as_str().unwrap().trim_end_matches(';');
            let (privileges, rest) = grant
                .strip_prefix("GRANT ")
                .unwrap()
                .split_once(" ON ")
                .unwrap();
            let roles = rest.split_once(" TO ").unwrap().1;
            for role in roles.split(',').map(str::trim) {
                if let Some(columns) = privileges
                    .strip_prefix("UPDATE (")
                    .and_then(|s| s.strip_suffix(')'))
                {
                    for column in columns.split(',') {
                        declared.insert(format!("{role}:{column}:UPDATE"));
                    }
                } else {
                    for privilege in privileges.split(',').map(str::trim) {
                        declared.insert(format!("{role}::{privilege}"));
                    }
                }
            }
        }
        let actual = rows("SELECT coalesce(r.rolname::text,'PUBLIC')||'::'||a.privilege_type AS g FROM pg_class c CROSS JOIN LATERAL aclexplode(c.relacl) a LEFT JOIN pg_roles r ON r.oid=a.grantee WHERE c.oid=$1::regclass AND (a.grantee=0 OR r.rolname<>c.relowner::regrole::text) UNION ALL SELECT coalesce(r.rolname::text,'PUBLIC')||':'||t.attname||':'||a.privilege_type FROM pg_attribute t CROSS JOIN LATERAL aclexplode(t.attacl) a LEFT JOIN pg_roles r ON r.oid=a.grantee WHERE t.attrelid=$1::regclass AND t.attnum>0", name).await?;
        let granted: BTreeSet<String> =
            actual.iter().map(|r| r.try_get("", "g").unwrap()).collect();
        assert_eq!(granted, declared, "{name} privileges");
    }
    // Sentinel: the privilege query observes an undeclared grant, so equality above is meaningful.
    pg.sql("GRANT DELETE ON bss_orders__order_version TO bss_orders_business")
        .await?;
    assert_eq!(pg.scalar("SELECT count(*) AS n FROM pg_class c CROSS JOIN LATERAL aclexplode(c.relacl) a JOIN pg_roles r ON r.oid=a.grantee WHERE c.oid='bss_orders__order_version'::regclass AND r.rolname='bss_orders_business' AND a.privilege_type='DELETE'").await?, 1);
    Ok(())
}
