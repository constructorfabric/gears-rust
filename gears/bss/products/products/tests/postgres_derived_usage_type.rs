#![allow(clippy::expect_used, clippy::unwrap_used)]
//! The derived usage type store on `PostgreSQL` (P-D-231, `m20261001_000012`): a version is
//! append-only by its trigger, its foreign key is tenant-qualified, and the repository's refusals
//! are the typed ones on this engine too. The `SQLite` twin is the migration's `_tests.rs`.
mod pg_support;

use bss_products::{
    domain::derived::{NewDerivedType, NewDerivedVersion},
    infra::storage::{RepoError, RepoRefusal, repo::derived_usage_type_repo as store},
};
use pg_support::Pg;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use time::OffsetDateTime;
use toolkit_db::secure::AccessScope;
use uuid::Uuid;

const REFUSED: &str = "products_derived_usage_type_version is append-only";

fn sql(statement: &str) -> Statement {
    Statement::from_string(DbBackend::Postgres, statement.to_owned())
}

struct Fixture {
    pg: Pg,
    db: toolkit_db::Db,
    tenant: Uuid,
    scope: AccessScope,
}
impl Fixture {
    async fn new() -> Self {
        let pg = Pg::applied().await;
        let db = pg.db().await;
        let tenant = Uuid::new_v4();
        Self {
            pg,
            db,
            tenant,
            scope: AccessScope::for_tenant(tenant),
        }
    }
    /// A type with versions 1 and 2.
    async fn seeded(&self) -> Uuid {
        let conn = self.db.conn().unwrap();
        let now = OffsetDateTime::now_utc();
        let t = store::create_type(
            &conn,
            &self.scope,
            self.tenant,
            NewDerivedType {
                code: "cloudlets".into(),
                name: "Cloudlets".into(),
            },
            Uuid::from_u128(7),
            now,
        )
        .await
        .unwrap();
        for version in [1, 2] {
            store::insert_version(
                &conn,
                &self.scope,
                self.tenant,
                NewDerivedVersion {
                    type_id: t.id,
                    version,
                    declaration_json: serde_json::json!({ "v": version }),
                    digest: format!("{version}").repeat(64),
                    created_by: Uuid::from_u128(7),
                    created_at: now,
                },
            )
            .await
            .unwrap();
        }
        t.id
    }
}

/// The trigger refuses every UPDATE and DELETE of a version, a no-op UPDATE included, and the
/// versions stay as written.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_version_refuses_update_and_delete() {
    let f = Fixture::new().await;
    let id = f.seeded().await;
    let raw = f.pg.raw().await;
    for statement in [
        "UPDATE bss.products_derived_usage_type_version SET digest = digest",
        "UPDATE bss.products_derived_usage_type_version SET declaration_json = '{}'::jsonb \
         WHERE version = 1",
        "DELETE FROM bss.products_derived_usage_type_version WHERE version = 2",
        "DELETE FROM bss.products_derived_usage_type_version",
    ] {
        let error = raw
            .execute_raw(sql(statement))
            .await
            .expect_err("a version mutation must fail");
        assert!(error.to_string().contains(REFUSED), "{statement}: {error}");
    }
    let versions = store::list_versions(&f.db.conn().unwrap(), &f.scope, f.tenant, id)
        .await
        .unwrap();
    let kept: Vec<_> = versions
        .iter()
        .map(|v| (v.version, v.digest.clone(), v.declaration_json.clone()))
        .collect();
    assert_eq!(
        kept,
        [
            (1, "1".repeat(64), serde_json::json!({"v": 1})),
            (2, "2".repeat(64), serde_json::json!({"v": 2}))
        ]
    );
    raw.close().await.unwrap();
}

/// The foreign key is tenant-qualified: a version naming the type under another tenant is refused
/// by `fk_products_derived_usage_type_version_type`.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_version_cannot_name_another_tenants_type() {
    let f = Fixture::new().await;
    let id = f.seeded().await;
    let other = Uuid::new_v4();
    let error = store::insert_version(
        &f.db.conn().unwrap(),
        &AccessScope::for_tenant(other),
        other,
        NewDerivedVersion {
            type_id: id,
            version: 3,
            declaration_json: serde_json::json!({}),
            digest: "a".repeat(64),
            created_by: Uuid::from_u128(7),
            created_at: OffsetDateTime::now_utc(),
        },
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("fk_products_derived_usage_type_version_type"),
        "{error}"
    );
}

/// The code index and the version key are the repository's typed refusals on this engine, and the
/// digest and code CHECKs hold.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_keys_are_typed_refusals_and_the_checks_hold() {
    let f = Fixture::new().await;
    let id = f.seeded().await;
    let conn = f.db.conn().unwrap();
    let taken = store::create_type(
        &conn,
        &f.scope,
        f.tenant,
        NewDerivedType {
            code: "cloudlets".into(),
            name: "Again".into(),
        },
        Uuid::from_u128(7),
        OffsetDateTime::now_utc(),
    )
    .await;
    assert!(
        matches!(
            taken,
            Err(RepoError::Refused(RepoRefusal::DerivedCodeTaken))
        ),
        "{taken:?}"
    );
    let again = store::insert_version(
        &conn,
        &f.scope,
        f.tenant,
        NewDerivedVersion {
            type_id: id,
            version: 2,
            declaration_json: serde_json::json!({}),
            digest: "a".repeat(64),
            created_by: Uuid::from_u128(7),
            created_at: OffsetDateTime::now_utc(),
        },
    )
    .await;
    assert!(
        matches!(
            again,
            Err(RepoError::Refused(RepoRefusal::DerivedVersionTaken))
        ),
        "{again:?}"
    );
    let raw = f.pg.raw().await;
    for (statement, check) in [
        (
            format!(
                "INSERT INTO bss.products_derived_usage_type_version VALUES ('{}', '{id}', 3, \
                 '{{}}', 'ABC', '{}', now())",
                f.tenant,
                Uuid::from_u128(7)
            ),
            "chk_products_derived_usage_type_version_digest",
        ),
        (
            format!(
                "INSERT INTO bss.products_derived_usage_type VALUES ('{}', '{}', 'Bad Code', 'N', \
                 '{}', now())",
                f.tenant,
                Uuid::new_v4(),
                Uuid::from_u128(7)
            ),
            "chk_products_derived_usage_type_code",
        ),
    ] {
        let error = raw.execute_raw(sql(&statement)).await.unwrap_err();
        assert!(error.to_string().contains(check), "{statement}: {error}");
    }
    raw.close().await.unwrap();
}

/// The list's grouped read returns the latest version row on Postgres, not only its number
/// (P-D-257). Another tenant reads none of it.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_latest_version_read_returns_the_latest_row() {
    let f = Fixture::new().await;
    let id = f.seeded().await;
    let conn = f.db.conn().unwrap();
    let latest = store::latest_versions(&conn, &f.scope, f.tenant, &[id])
        .await
        .unwrap();
    assert_eq!(latest.len(), 1);
    let row = &latest[&id];
    assert_eq!(row.version, 2);
    assert_eq!(row.digest, "2".repeat(64));
    assert_eq!(row.declaration_json, serde_json::json!({ "v": 2 }));
    let other = Uuid::new_v4();
    assert!(
        store::latest_versions(&conn, &AccessScope::for_tenant(other), other, &[id])
            .await
            .unwrap()
            .is_empty()
    );
}
