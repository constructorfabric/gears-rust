#![allow(clippy::expect_used, clippy::unwrap_used)]
//! The quota catalog store on `PostgreSQL` (P-D-274, the quota migration 000015): the evidence
//! tables are append-only by their triggers, `TRUNCATE` included, a candidate moves only its review
//! columns until it is decided, the migration reverts and applies again, the keys are the
//! repositories' typed refusals, two concurrent review writes move one, and the definition list
//! answers the versions applicable up to a SKU version. The `SQLite` twins are the migration's and
//! the repositories' `_tests.rs`.
mod pg_support;

use std::sync::Arc;

use bss_products::{
    api::rest::{ApiState, quota::candidates::write_manual, quota::discovery::stage_snapshot},
    domain::quota::{
        Availability, CandidateOrigin, CandidateState, DefinitionFilter, DefinitionVersion,
        MappingVersion, NewCandidate, NewQuotaTarget, QuotaCandidate, QuotaTarget, ReviewWrite,
        StoredSnapshot,
        evidence::EvidenceSet,
        intake::{IntakePlan, plan_intake},
        record::JudgedRecord,
        snapshot::VerifiedSnapshot,
    },
    gear::BssProductsGear,
    infra::storage::{
        RepoError, RepoRefusal, repo::quota_catalog_repo as catalog,
        repo::quota_intake_repo as intake, stored_now,
    },
};
use bss_products_sdk::quota::catalog::{QuotaAvailabilityState, QuotaVersion};
use bss_products_sdk::quota::digest::{record_digest, snapshot_digest};
use pg_support::{Pg, wait_until_a_backend_blocks};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::json;
use time::OffsetDateTime;
use tokio::sync::{Barrier, Notify};
use toolkit::contracts::DatabaseCapability;
use toolkit_db::secure::AccessScope;
use toolkit_odata::ODataQuery;
use uuid::Uuid;

const MIGRATION: &str = "m20261119_000015_quota_catalog";

const QUOTA_TABLES: [&str; 7] = [
    "products_quota_adapter_target",
    "products_quota_snapshot",
    "products_quota_candidate",
    "products_quota_mapping_version",
    "products_quota_definition_version",
    "products_quota_definition_resource_type",
    "products_quota_availability",
];

/// The quota tables, their trigger functions, their triggers, their indexes and the unique lineage
/// index, in schema `bss`.
const CENSUS: [&str; 5] = [
    "SELECT count(*) AS n FROM information_schema.tables WHERE table_schema = 'bss' \
     AND table_name LIKE 'products\\_quota\\_%'",
    "SELECT count(*) AS n FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
     WHERE n.nspname = 'bss' AND p.proname LIKE 'products\\_quota\\_%'",
    "SELECT count(*) AS n FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid \
     JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'bss' \
     AND c.relname LIKE 'products\\_quota\\_%' AND NOT t.tgisinternal",
    "SELECT count(*) AS n FROM pg_indexes WHERE schemaname = 'bss' \
     AND tablename LIKE 'products\\_quota\\_%'",
    "SELECT count(*) AS n FROM pg_indexes WHERE schemaname = 'bss' \
     AND indexname = 'uq_products_quota_mapping_version_lineage' \
     AND indexdef LIKE 'CREATE UNIQUE INDEX %'",
];

fn sql(statement: &str) -> Statement {
    Statement::from_string(DbBackend::Postgres, statement.to_owned())
}

fn v(n: u64) -> QuotaVersion {
    QuotaVersion::new(n).unwrap()
}

struct Fixture {
    pg: Pg,
    db: toolkit_db::Db,
    tenant: Uuid,
    scope: AccessScope,
    actor: Uuid,
    now: OffsetDateTime,
}

/// One row in every quota table.
struct Seeded {
    target: QuotaTarget,
    snapshot: StoredSnapshot,
    candidate: QuotaCandidate,
    definition: Uuid,
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
            actor: Uuid::new_v4(),
            now: stored_now(),
        }
    }

    async fn target(&self, key: &str) -> QuotaTarget {
        intake::insert_target(
            &self.db.conn().unwrap(),
            &self.scope,
            self.tenant,
            NewQuotaTarget {
                plugin_instance_id: "plugin-instance".to_owned(),
                target_key: key.to_owned(),
                name: format!("Pool {key}"),
            },
            self.actor,
            self.now,
        )
        .await
        .unwrap()
    }

    fn snapshot(&self, target: Uuid, revision: &str) -> StoredSnapshot {
        StoredSnapshot {
            id: Uuid::new_v4(),
            target_id: target,
            snapshot_id: Uuid::new_v4(),
            source_revision: revision.to_owned(),
            observed_at: self.now,
            item_count: 1,
            content_digest: "a".repeat(64),
            snapshot_json: json!({"source_revision": revision}),
            evidence_json: json!({}),
            review_json: json!({"rejected": [], "drift": [], "retired": []}),
            created_by: self.actor,
            created_at: self.now,
        }
    }

    async fn candidate(&self, target: Uuid, snapshot: Option<Uuid>, key: &str) -> QuotaCandidate {
        let record_id = Uuid::new_v4();
        intake::insert_candidates(
            &self.db.conn().unwrap(),
            &self.scope,
            self.tenant,
            &[NewCandidate {
                target_id: target,
                origin: if snapshot.is_some() {
                    CandidateOrigin::Discovery
                } else {
                    CandidateOrigin::Manual
                },
                snapshot_row_id: snapshot,
                record_id,
                capability_key: key.to_owned(),
                record_json: json!({"id": record_id.to_string()}),
                record_digest: "b".repeat(64),
                evidence_json: json!({}),
                created_by: self.actor,
                created_at: self.now,
            }],
        )
        .await
        .unwrap()
        .remove(0)
    }

    /// A definition version 1 of a fresh candidate, at `sku_version`, with its availability
    /// revision 1 disabled until qualification.
    async fn definition(&self, sku: Uuid, sku_version: u64, resource_type: Uuid) -> Uuid {
        let target = self.target(&Uuid::new_v4().to_string()).await;
        let candidate = self.candidate(target.id, None, "tenant-vcpu").await;
        self.definition_of(&candidate, sku, sku_version, resource_type)
            .await
    }

    async fn definition_of(
        &self,
        candidate: &QuotaCandidate,
        sku: Uuid,
        sku_version: u64,
        resource_type: Uuid,
    ) -> Uuid {
        let conn = self.db.conn().unwrap();
        let mapping = MappingVersion {
            mapping_id: Uuid::new_v4(),
            version: v(1),
            target_id: candidate.target_id,
            capability_key: candidate.capability_key.clone(),
            plugin_instance_id: "plugin-instance".to_owned(),
            target_key: "pool".to_owned(),
            mapping_key: "tenant-vcpu-mapping-3".to_owned(),
            mapping_json: json!({}),
            digest: "e".repeat(64),
            profile_id: Uuid::new_v4(),
            profile_version: v(1),
            profile_json: json!({}),
            profile_digest: "f".repeat(64),
            created_by: self.actor,
            created_at: self.now,
        };
        catalog::insert_mapping(&conn, &self.scope, self.tenant, &mapping)
            .await
            .unwrap();
        let id = Uuid::new_v4();
        catalog::insert_definition(
            &conn,
            &self.scope,
            self.tenant,
            &DefinitionVersion {
                definition_id: id,
                version: v(1),
                candidate_id: candidate.id,
                sku_id: sku,
                sku_version: v(sku_version),
                metric_uuid: Uuid::from_u128(0x3c),
                subject_type_uuid: Uuid::from_u128(0x5b),
                profile_id: mapping.profile_id,
                mapping_id: mapping.mapping_id,
                mapping_version: mapping.version,
                content_json: json!({}),
                digest: "0".repeat(64),
                created_by: self.actor,
                created_at: self.now,
            },
            &[resource_type],
        )
        .await
        .unwrap();
        self.availability(id, 1, QuotaAvailabilityState::Disabled)
            .await
            .unwrap();
        id
    }

    async fn availability(
        &self,
        definition: Uuid,
        revision: u64,
        state: QuotaAvailabilityState,
    ) -> Result<(), RepoError> {
        catalog::insert_availability(
            &self.db.conn().unwrap(),
            &self.scope,
            self.tenant,
            &Availability {
                definition_id: definition,
                version: v(1),
                revision: v(revision),
                state,
                reason_code: (state != QuotaAvailabilityState::Enabled)
                    .then(|| "qualification_pending".to_owned()),
                qualification: None,
                observed_at: self.now,
                decided_by: self.actor,
            },
        )
        .await
    }

    async fn seeded(&self) -> Seeded {
        let target = self.target("pool-a").await;
        let snapshot = self.snapshot(target.id, "7");
        intake::insert_snapshot(
            &self.db.conn().unwrap(),
            &self.scope,
            self.tenant,
            &snapshot,
        )
        .await
        .unwrap();
        let candidate = self
            .candidate(target.id, Some(snapshot.id), "tenant-vcpu")
            .await;
        let definition = self
            .definition_of(&candidate, Uuid::new_v4(), 1, Uuid::from_u128(0x3e))
            .await;
        Seeded {
            target,
            snapshot,
            candidate,
            definition,
        }
    }

    async fn listed(&self, filter: &DefinitionFilter) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = catalog::list_definitions(
            &self.db.conn().unwrap(),
            &self.scope,
            self.tenant,
            filter,
            &ODataQuery::default(),
        )
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|(d, _)| d.definition_id)
        .collect();
        ids.sort();
        ids
    }
}

fn filter(sku: Uuid, sku_version: u64) -> DefinitionFilter {
    DefinitionFilter {
        sku_id: sku,
        sku_version: v(sku_version),
        subject_type_uuid: None,
        resource_type_uuid: None,
        profile_id: None,
        enabled_only: false,
    }
}

/// Every trigger refuses every `UPDATE` and `DELETE` of its table, a no-op `UPDATE` included.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn every_evidence_table_refuses_update_and_delete_on_postgres() {
    let f = Fixture::new().await;
    let seeded = f.seeded().await;
    let raw = f.pg.raw().await;
    for (table, key) in [
        ("products_quota_adapter_target", "id"),
        ("products_quota_snapshot", "id"),
        ("products_quota_mapping_version", "mapping_id"),
        ("products_quota_definition_version", "definition_id"),
        ("products_quota_definition_resource_type", "definition_id"),
        ("products_quota_availability", "definition_id"),
    ] {
        for (statement, verb) in [
            (format!("UPDATE bss.{table} SET {key} = {key}"), "UPDATE"),
            (format!("DELETE FROM bss.{table}"), "DELETE"),
        ] {
            let error = raw
                .execute_raw(sql(&statement))
                .await
                .expect_err("an evidence mutation must fail")
                .to_string();
            assert!(
                error.contains(&format!("{table} is append-only: {verb} is not permitted")),
                "{statement}: {error}"
            );
        }
    }
    let conn = f.db.conn().unwrap();
    assert_eq!(
        intake::find_target(&conn, &f.scope, f.tenant, seeded.target.id)
            .await
            .unwrap(),
        Some(seeded.target)
    );
    assert_eq!(
        intake::snapshot_at(&conn, &f.scope, f.tenant, seeded.snapshot.target_id, "7")
            .await
            .unwrap(),
        Some(seeded.snapshot)
    );
    assert!(
        catalog::find_definition(&conn, &f.scope, f.tenant, seeded.definition, v(1))
            .await
            .unwrap()
            .is_some()
    );
    raw.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_candidate_moves_only_its_review_columns_on_postgres() {
    let f = Fixture::new().await;
    let seeded = f.seeded().await;
    let raw = f.pg.raw().await;
    raw.execute_raw(sql(&format!(
        "UPDATE bss.products_quota_candidate SET state = 'submitted', version = 2, \
         proposal_json = '{{}}'::jsonb, submitted_by = '{}', submitted_at = now(), \
         submit_note = 'n' WHERE id = '{}'",
        f.actor, seeded.candidate.id
    )))
    .await
    .unwrap();
    for set in [
        "tenant_id = gen_random_uuid()",
        "id = gen_random_uuid()",
        "target_id = gen_random_uuid()",
        "origin = 'manual'",
        "snapshot_row_id = NULL",
        "record_id = gen_random_uuid()",
        "capability_key = 'other'",
        "record_json = '{\"other\": 1}'::jsonb",
        "record_digest = repeat('1', 64)",
        "evidence_json = '{\"other\": 1}'::jsonb",
        "created_by = gen_random_uuid()",
        "created_at = now()",
    ] {
        let error = raw
            .execute_raw(sql(&format!(
                "UPDATE bss.products_quota_candidate SET {set}"
            )))
            .await
            .expect_err("a content column must not move")
            .to_string();
        assert!(
            error.contains("products_quota_candidate content is immutable"),
            "{set}: {error}"
        );
    }
    let decider = Uuid::new_v4();
    raw.execute_raw(sql(&format!(
        "UPDATE bss.products_quota_candidate SET state = 'approved', version = 3, \
         decided_by = '{decider}', decided_at = now(), decided_note = 'ok', \
         definition_id = '{}', definition_version = 1 WHERE id = '{}'",
        seeded.definition, seeded.candidate.id
    )))
    .await
    .unwrap();
    let error = raw
        .execute_raw(sql("DELETE FROM bss.products_quota_candidate"))
        .await
        .expect_err("a candidate is never deleted")
        .to_string();
    assert!(
        error.contains("products_quota_candidate is append-only: DELETE is not permitted"),
        "{error}"
    );
    let read = intake::find_candidate(
        &f.db.conn().unwrap(),
        &f.scope,
        f.tenant,
        seeded.candidate.id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        (
            read.state,
            read.version,
            read.decided_by,
            read.decided_note.as_deref(),
            read.definition_id,
            read.definition_version,
            read.record_json
        ),
        (
            CandidateState::Approved,
            3,
            Some(decider),
            Some("ok"),
            Some(seeded.definition),
            Some(v(1)),
            seeded.candidate.record_json
        )
    );
    raw.close().await.unwrap();
}

/// A decided candidate is frozen: once approved or rejected through the repository, no column
/// moves, its review columns included, and a no-op `UPDATE` is refused too.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_decided_candidate_is_frozen_on_postgres() {
    let f = Fixture::new().await;
    let target = f.target("pool-a").await;
    let conn = f.db.conn().unwrap();
    let mut decided = Vec::new();
    for (key, approve) in [("approved", true), ("rejected", false)] {
        let candidate = f.candidate(target.id, None, key).await;
        let submit = ReviewWrite::Submit {
            proposal: json!({}),
            evidence: json!({}),
            by: f.actor,
            at: f.now,
            note: None,
        };
        assert!(
            intake::review_candidate(&conn, &f.scope, f.tenant, candidate.id, 1, submit)
                .await
                .unwrap()
        );
        let decision = if approve {
            ReviewWrite::Approve {
                by: Uuid::new_v4(),
                at: f.now,
                note: None,
                definition_id: Uuid::new_v4(),
                definition_version: v(1),
            }
        } else {
            ReviewWrite::Reject {
                by: Uuid::new_v4(),
                at: f.now,
                note: "no".to_owned(),
            }
        };
        assert!(
            intake::review_candidate(&conn, &f.scope, f.tenant, candidate.id, 2, decision)
                .await
                .unwrap()
        );
        decided.push(candidate.id);
    }
    let raw = f.pg.raw().await;
    for id in decided {
        for set in [
            "state = state",
            "decided_note = 'changed'",
            "state = 'submitted', version = 4",
            "definition_id = NULL, definition_version = NULL",
            "proposal_json = '{\"other\": 1}'::jsonb",
        ] {
            let error = raw
                .execute_raw(sql(&format!(
                    "UPDATE bss.products_quota_candidate SET {set} WHERE id = '{id}'"
                )))
                .await
                .expect_err("a decided candidate must not move")
                .to_string();
            assert!(
                error.contains("products_quota_candidate is decided: UPDATE is not permitted"),
                "{set}: {error}"
            );
        }
    }
    raw.close().await.unwrap();
}

/// `TRUNCATE` fires no row trigger: every quota table refuses it by a statement trigger, a
/// `CASCADE` from a parent included, and every row is still there.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn no_quota_table_can_be_truncated_on_postgres() {
    let f = Fixture::new().await;
    let seeded = f.seeded().await;
    let raw = f.pg.raw().await;
    for table in QUOTA_TABLES {
        let error = raw
            .execute_raw(sql(&format!("TRUNCATE bss.{table} CASCADE")))
            .await
            .expect_err("a quota table must not be truncated")
            .to_string();
        assert!(
            error.contains(&format!(
                "{table} is append-only: TRUNCATE is not permitted"
            )),
            "{table}: {error}"
        );
    }
    let conn = f.db.conn().unwrap();
    assert_eq!(
        intake::find_candidate(&conn, &f.scope, f.tenant, seeded.candidate.id)
            .await
            .unwrap(),
        Some(seeded.candidate)
    );
    assert!(
        catalog::find_definition(&conn, &f.scope, f.tenant, seeded.definition, v(1))
            .await
            .unwrap()
            .is_some()
    );
    raw.close().await.unwrap();
}

/// The quota migration, as the gear lists it.
fn quota_migration() -> Box<dyn sea_orm_migration::MigrationTrait> {
    BssProductsGear::default()
        .migrations()
        .into_iter()
        .find(|m| m.name() == MIGRATION)
        .unwrap()
}

/// The counts of [`CENSUS`]: the quota tables, their trigger functions, their triggers, their
/// indexes (the primary keys' included) and the unique lineage index.
async fn census(raw: &sea_orm::DatabaseConnection) -> [i64; CENSUS.len()] {
    let mut counts = [0_i64; CENSUS.len()];
    for (count, statement) in counts.iter_mut().zip(CENSUS) {
        *count = raw
            .query_one_raw(sql(statement))
            .await
            .unwrap()
            .unwrap()
            .try_get::<i64>("", "n")
            .unwrap();
    }
    counts
}

/// The migration reverts on the engine production runs while its tables are empty: `down` drops
/// every quota table and trigger function, and `up` creates them again with their guards and
/// their indexes, the unique lineage index included.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_quota_migration_reverts_and_applies_again_on_postgres() {
    let f = Fixture::new().await;
    let raw = f.pg.raw().await;
    let manager = sea_orm_migration::SchemaManager::new(&raw);
    let migration = quota_migration();
    let applied = census(&raw).await;
    assert_eq!(
        applied,
        [7, 7, 14, 18, 1],
        "the tables, their trigger functions, a row and a statement trigger per table, a primary \
         key and eleven indexes, and the unique lineage index"
    );
    migration.down(&manager).await.unwrap();
    assert_eq!(census(&raw).await, [0, 0, 0, 0, 0]);
    migration.down(&manager).await.unwrap();
    migration.up(&manager).await.unwrap();
    assert_eq!(census(&raw).await, applied);
    let target = f.target("pool-a").await;
    let error = raw
        .execute_raw(sql(&format!(
            "UPDATE bss.products_quota_adapter_target SET name = 'x' WHERE id = '{}'",
            target.id
        )))
        .await
        .expect_err("the guards hold again")
        .to_string();
    assert!(error.contains("is append-only: UPDATE"), "{error}");
    raw.close().await.unwrap();
}

/// `down` refuses while any quota row exists, before it drops anything: one target row names its
/// table; a row in every table names every table, and every row is kept.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_quota_migration_refuses_to_revert_while_a_row_exists_on_postgres() {
    let f = Fixture::new().await;
    let raw = f.pg.raw().await;
    let manager = sea_orm_migration::SchemaManager::new(&raw);
    let migration = quota_migration();
    let applied = census(&raw).await;
    f.target("pool-b").await;
    let error = migration
        .down(&manager)
        .await
        .expect_err("one stored row refuses the revert")
        .to_string();
    assert!(
        error.contains(MIGRATION) && error.contains("products_quota_adapter_target holds a row"),
        "{error}"
    );
    assert!(!error.contains("products_quota_snapshot"), "{error}");
    assert_eq!(census(&raw).await, applied, "nothing is dropped");
    f.seeded().await;
    let error = migration
        .down(&manager)
        .await
        .expect_err("stored rows refuse the revert")
        .to_string();
    for table in QUOTA_TABLES {
        assert!(
            error.contains(&format!("{table} holds a row")),
            "{table}: {error}"
        );
        let held = raw
            .query_one_raw(sql(&format!("SELECT count(*) AS n FROM bss.{table}")))
            .await
            .unwrap()
            .unwrap()
            .try_get::<i64>("", "n")
            .unwrap();
        assert!(held >= 1, "{table} keeps its rows");
    }
    assert_eq!(census(&raw).await, applied, "nothing is dropped");
    raw.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_second_snapshot_at_a_revision_is_refused_as_a_conflict_on_postgres() {
    let f = Fixture::new().await;
    let seeded = f.seeded().await;
    let conn = f.db.conn().unwrap();
    let mut again = f.snapshot(seeded.target.id, "7");
    again.content_digest = "c".repeat(64);
    let refused = intake::insert_snapshot(&conn, &f.scope, f.tenant, &again).await;
    assert!(
        matches!(
            refused,
            Err(RepoError::Refused(RepoRefusal::QuotaSnapshotTaken))
        ),
        "{refused:?}"
    );
    let taken = intake::insert_target(
        &conn,
        &f.scope,
        f.tenant,
        NewQuotaTarget {
            plugin_instance_id: "plugin-instance".to_owned(),
            target_key: "pool-a".to_owned(),
            name: "Again".to_owned(),
        },
        f.actor,
        f.now,
    )
    .await;
    assert!(
        matches!(
            taken,
            Err(RepoError::Refused(RepoRefusal::QuotaTargetTaken))
        ),
        "{taken:?}"
    );
    let manual = f.candidate(seeded.target.id, None, "k").await;
    let duplicate = intake::insert_candidates(
        &conn,
        &f.scope,
        f.tenant,
        &[NewCandidate {
            target_id: manual.target_id,
            origin: CandidateOrigin::Manual,
            snapshot_row_id: None,
            record_id: manual.record_id,
            capability_key: "k".to_owned(),
            record_json: json!({}),
            record_digest: "b".repeat(64),
            evidence_json: json!({}),
            created_by: f.actor,
            created_at: f.now,
        }],
    )
    .await;
    assert!(
        matches!(
            duplicate,
            Err(RepoError::Refused(RepoRefusal::QuotaManualRecordTaken))
        ),
        "{duplicate:?}"
    );
    let revision = f
        .availability(seeded.definition, 1, QuotaAvailabilityState::Disabled)
        .await;
    assert!(
        matches!(
            revision,
            Err(RepoError::Refused(RepoRefusal::QuotaApprovalContended))
        ),
        "{revision:?}"
    );
}

/// Two review writes at the same seen version, each in its own transaction on its own pool: the
/// one that writes first moves the candidate, the other waits on the row and then moves nothing.
/// Run with either side first.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_concurrent_review_writes_move_one_on_postgres() {
    let f = Fixture::new().await;
    let target = f.target("pool-a").await;
    for leader in [0_usize, 1] {
        let candidate = f.candidate(target.id, None, &format!("k{leader}")).await;
        let barrier = Arc::new(Barrier::new(2));
        let written = Arc::new(Notify::new());
        let observer = Arc::new(f.pg.raw().await);
        let side = |me: usize, db: toolkit_db::Db| {
            let (barrier, written, observer) = (barrier.clone(), written.clone(), observer.clone());
            let (scope, tenant, id, now) = (f.scope.clone(), f.tenant, candidate.id, f.now);
            async move {
                db.transaction_ref(move |tx| {
                    Box::pin(async move {
                        let seen = intake::find_candidate(tx, &scope, tenant, id)
                            .await
                            .unwrap()
                            .unwrap()
                            .version;
                        barrier.wait().await;
                        if me != leader {
                            written.notified().await;
                        }
                        let moved = intake::review_candidate(
                            tx,
                            &scope,
                            tenant,
                            id,
                            seen,
                            ReviewWrite::Submit {
                                proposal: json!({"side": me}),
                                evidence: json!({}),
                                by: Uuid::new_v4(),
                                at: now,
                                note: None,
                            },
                        )
                        .await
                        .unwrap();
                        if me == leader {
                            written.notify_one();
                            wait_until_a_backend_blocks(&observer).await;
                        }
                        Ok(moved)
                    })
                })
                .await
                .unwrap()
            }
        };
        let (zero, one) = tokio::join!(side(0, f.pg.db().await), side(1, f.pg.db().await));
        let moved = [zero, one];
        assert_eq!(
            moved.iter().filter(|m| **m).count(),
            1,
            "leader {leader}: {moved:?}"
        );
        assert!(moved[leader], "leader {leader}: {moved:?}");
        let read = intake::find_candidate(&f.db.conn().unwrap(), &f.scope, f.tenant, candidate.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (read.state, read.version, read.proposal_json),
            (CandidateState::Submitted, 2, Some(json!({"side": leader})))
        );
    }
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_definition_list_answers_versions_up_to_the_sku_version_on_postgres() {
    let f = Fixture::new().await;
    let sku = Uuid::new_v4();
    let (vm, volume) = (Uuid::from_u128(0x1), Uuid::from_u128(0x2));
    let at_two = f.definition(sku, 2, vm).await;
    let at_four = f.definition(sku, 4, volume).await;
    let _other_sku = f.definition(Uuid::new_v4(), 1, vm).await;
    assert_eq!(f.listed(&filter(sku, 1)).await, Vec::<Uuid>::new());
    assert_eq!(f.listed(&filter(sku, 3)).await, vec![at_two]);
    let mut both = vec![at_two, at_four];
    both.sort();
    assert_eq!(f.listed(&filter(sku, 4)).await, both);
    assert_eq!(
        f.listed(&DefinitionFilter {
            resource_type_uuid: Some(volume),
            ..filter(sku, 4)
        })
        .await,
        vec![at_four]
    );
    f.availability(at_two, 2, QuotaAvailabilityState::Enabled)
        .await
        .unwrap();
    let enabled_only = DefinitionFilter {
        enabled_only: true,
        ..filter(sku, 4)
    };
    assert_eq!(f.listed(&enabled_only).await, vec![at_two]);
    f.availability(at_two, 3, QuotaAvailabilityState::Withdrawn)
        .await
        .unwrap();
    assert_eq!(f.listed(&enabled_only).await, Vec::<Uuid>::new());
    let (_, current) =
        catalog::find_definition(&f.db.conn().unwrap(), &f.scope, f.tenant, at_two, v(1))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        (current.revision, current.state),
        (v(3), QuotaAvailabilityState::Withdrawn)
    );
}

/// The digests of a page's approved candidates in one read: each stored version's digest, and
/// nothing for a version not stored.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_definition_digests_answer_each_stored_version_on_postgres() {
    let f = Fixture::new().await;
    let sku = Uuid::new_v4();
    let stored = [
        f.definition(sku, 1, Uuid::from_u128(0x1)).await,
        f.definition(sku, 2, Uuid::from_u128(0x2)).await,
    ];
    let mut keys: Vec<(Uuid, QuotaVersion)> = stored.iter().map(|id| (*id, v(1))).collect();
    keys.push((Uuid::new_v4(), v(1)));
    keys.push((stored[0], v(2)));
    let digests = catalog::definition_digests(&f.db.conn().unwrap(), &f.scope, f.tenant, &keys)
        .await
        .unwrap();
    assert_eq!(digests.len(), 2, "{digests:?}");
    for id in stored {
        assert_eq!(digests.get(&(id, v(1))), Some(&"0".repeat(64)));
    }
    let other = AccessScope::for_tenant(Uuid::new_v4());
    let foreign = catalog::definition_digests(&f.db.conn().unwrap(), &other, f.tenant, &keys)
        .await
        .unwrap();
    assert!(foreign.is_empty(), "another tenant's scope reads none");
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_highest_revision_compares_as_a_number_on_postgres() {
    let f = Fixture::new().await;
    let target = f.target("pool-a").await;
    let conn = f.db.conn().unwrap();
    for revision in ["9", "10", "2"] {
        intake::insert_snapshot(&conn, &f.scope, f.tenant, &f.snapshot(target.id, revision))
            .await
            .unwrap();
    }
    assert_eq!(
        intake::highest_revision(&conn, &f.scope, f.tenant, target.id)
            .await
            .unwrap()
            .as_deref(),
        Some("10")
    );
}

/// The CHECKs hold on this engine, and no child row names another tenant's parent.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_checks_and_tenant_keys_hold_on_postgres() {
    let f = Fixture::new().await;
    let seeded = f.seeded().await;
    // A candidate no definition names yet, so only the CHECK can refuse its definition rows.
    let fresh = f.candidate(seeded.target.id, None, "tenant-ram").await;
    let raw = f.pg.raw().await;
    let definition = |version: u64, sku_version: u64| {
        format!(
            "INSERT INTO bss.products_quota_definition_version SELECT tenant_id, \
             gen_random_uuid(), {version}, '{}', sku_id, {sku_version}, metric_uuid, \
             subject_type_uuid, profile_id, mapping_id, mapping_version, content_json, digest, \
             created_by, created_at FROM bss.products_quota_definition_version \
             WHERE definition_id = '{}'",
            fresh.id, seeded.definition
        )
    };
    let snapshot = |tenant: Uuid, revision: &str, digest: &str| {
        format!(
            "INSERT INTO bss.products_quota_snapshot VALUES ('{tenant}', gen_random_uuid(), '{}', \
             gen_random_uuid(), '{revision}', now(), 1, '{digest}', '{{}}', '{{}}', '{{}}', \
             '{}', now())",
            seeded.target.id, f.actor
        )
    };
    for (statement, refusal) in [
        (
            snapshot(f.tenant, "07", &"a".repeat(64)),
            "chk_products_quota_snapshot_revision",
        ),
        (
            snapshot(f.tenant, "8", &"A".repeat(64)),
            "chk_products_quota_snapshot_digest",
        ),
        (
            snapshot(Uuid::new_v4(), "8", &"a".repeat(64)),
            "fk_products_quota_snapshot_target",
        ),
        (
            format!(
                "INSERT INTO bss.products_quota_candidate (tenant_id, id, target_id, origin, \
                 snapshot_row_id, record_id, capability_key, record_json, record_digest, \
                 evidence_json, state, version, created_by, created_at) VALUES ('{}', \
                 gen_random_uuid(), '{}', 'manual', '{}', gen_random_uuid(), 'k', '{{}}', \
                 repeat('b', 64), '{{}}', 'staged', 1, '{}', now())",
                f.tenant, seeded.target.id, seeded.snapshot.id, f.actor
            ),
            "chk_products_quota_candidate_origin_snapshot",
        ),
        (
            definition(0, 1),
            "chk_products_quota_definition_version_version",
        ),
        (
            definition(1, 0),
            "chk_products_quota_definition_version_version",
        ),
        (
            format!(
                "INSERT INTO bss.products_quota_availability VALUES ('{}', '{}', 1, 9, \
                 'enabled', 'qualification_pending', NULL, NULL, NULL, now(), '{}')",
                f.tenant, seeded.definition, f.actor
            ),
            "chk_products_quota_availability_reason",
        ),
        (
            format!(
                "INSERT INTO bss.products_quota_availability VALUES ('{}', '{}', 1, 9, \
                 'disabled', 'x', gen_random_uuid(), NULL, NULL, now(), '{}')",
                f.tenant, seeded.definition, f.actor
            ),
            "chk_products_quota_availability_qualification",
        ),
        (
            format!(
                "INSERT INTO bss.products_quota_availability VALUES ('{}', '{}', 1, 9, \
                 'disabled', 'x', NULL, NULL, NULL, now(), '{}')",
                Uuid::new_v4(),
                seeded.definition,
                f.actor
            ),
            "fk_products_quota_availability_definition",
        ),
    ] {
        let error = raw
            .execute_raw(sql(&statement))
            .await
            .expect_err("the row must be refused")
            .to_string();
        assert!(error.contains(refusal), "{refusal}: {error}");
    }
    raw.close().await.unwrap();
}

/// A package example file, as the adapter registers or discovers it.
fn package(name: &str) -> serde_json::Value {
    serde_json::from_str(match name {
        "units" => include_str!("../../../docs/quotas/schemas/quota-metric-unit.instances.json"),
        "policies" => include_str!("../../../docs/quotas/schemas/quota-policies.instances.json"),
        _ => include_str!("../../../docs/quotas/schemas/quota-capability-discovery.example.json"),
    })
    .unwrap()
}

/// The package's example record, judged: its instances as the example adapter registers them.
fn judged_example() -> JudgedRecord {
    let raw = package("snapshot")["items"][0].clone();
    let (units, policies) = (package("units"), package("policies"));
    let unit: bss_products_sdk::quota::gts::QuotaUnitV1 =
        serde_json::from_value(units[0].clone()).unwrap();
    JudgedRecord {
        record: serde_json::from_value(raw.clone()).unwrap(),
        record_digest: record_digest(&raw),
        raw,
        evidence: EvidenceSet::default(),
        metric: serde_json::from_value(units[1].clone()).unwrap(),
        canonical_unit: unit.clone(),
        unit,
        accounting: serde_json::from_value(policies[0].clone()).unwrap(),
        scope: serde_json::from_value(policies[1].clone()).unwrap(),
    }
}

/// The example record's snapshot at `revision`, verified, with `max` as its bound.
fn verified(revision: &str, max: &str) -> (VerifiedSnapshot, IntakePlan) {
    let mut judged = judged_example();
    judged.raw["limits"]["max"] = json!(max);
    judged.record_digest = record_digest(&judged.raw);
    let items = vec![judged.raw.clone()];
    let observed_at = OffsetDateTime::from_unix_timestamp(1_794_571_200).unwrap();
    let snapshot_id = Uuid::from_u128(0x9144_d0b7_3497_4db9_89e7_69c2_c5d8_f035);
    let snapshot = VerifiedSnapshot {
        snapshot_id,
        source_revision: revision.to_owned(),
        observed_at,
        item_count: 1,
        content_digest: snapshot_digest(snapshot_id, revision, observed_at, 1, &items).unwrap(),
        items,
    };
    (snapshot, plan_intake(vec![Ok(judged)], &[]))
}

/// A state over its own pool, sharing one holding outbox: a staging needs only its database.
fn api_state(db: toolkit_db::Db, outbox: &toolkit_db::outbox::OutboxHandle) -> Arc<ApiState> {
    let hub = Arc::new(toolkit::ClientHub::new());
    Arc::new(ApiState {
        db: toolkit_db::DBProvider::new(db),
        sink: bss_products::infra::broker::EventSink::Interim(Arc::clone(outbox.outbox())),
        usage_type_catalog: Arc::new(bss_products::infra::usage_types::UnconfiguredUsageTypes),
        usage_type_catalog_source: "unconfigured",
        idempotency_retention_hours: 24,
        fence_ttl_minutes: 30,
        reference_principals: std::collections::BTreeMap::new(),
        actor_names: ApiState::names_from(&hub),
        hub,
    })
}

async fn holding_outbox(f: &Fixture) -> toolkit_db::outbox::OutboxHandle {
    let db = f.pg.db().await;
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        toolkit_db::outbox::outbox_migrations_with_prefix(
            bss_products::infra::events::OUTBOX_TABLE_PREFIX,
        )
        .unwrap(),
    )
    .await
    .unwrap();
    toolkit_db::outbox::Outbox::builder(db)
        .table_prefix(bss_products::infra::events::OUTBOX_TABLE_PREFIX)
        .unwrap()
        .queue(
            bss_products::infra::events::QUEUE_NAME,
            toolkit_db::outbox::Partitions::of(bss_products::infra::events::PARTITIONS),
        )
        .leased(bss_products::infra::events::PendingBrokerProducer)
        .start()
        .await
        .unwrap()
}

/// The status and the JSON body of what a staging answered, a refusal included.
async fn answered(
    staged: Result<axum::response::Response, impl axum::response::IntoResponse>,
) -> (u16, serde_json::Value) {
    let response = match staged {
        Ok(response) => response,
        Err(refusal) => refusal.into_response(),
    };
    let status = response.status().as_u16();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn snapshots_of(f: &Fixture, target: Uuid) -> i64 {
    f.pg.raw()
        .await
        .query_one_raw(sql(&format!(
            "SELECT count(*) AS n FROM bss.products_quota_snapshot WHERE target_id = '{target}'"
        )))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap()
}

/// The statuses of the answered receipts stored under the fixture's tenant and `key`.
async fn receipts_of(f: &Fixture, key: &str) -> Vec<i32> {
    f.pg.raw()
        .await
        .query_all_raw(sql(&format!(
            "SELECT response_status AS s FROM bss.products_idempotency WHERE tenant_id = '{}' \
             AND client_key = '{key}' AND state = 'answered'",
            f.tenant
        )))
        .await
        .unwrap()
        .iter()
        .map(|row| row.try_get::<i32>("", "s").unwrap())
        .collect()
}

/// A keyed staging, as the discovery runs it, whose insert waits behind another transaction's
/// uncommitted snapshot of the same revision: the other commits, Postgres refuses the waiting
/// insert and aborts its transaction, and the staging answers the winner from a fresh
/// transaction: a replay (200) of the same digest, which records the key's receipt, or a conflict
/// (409) of another, which records none. The same key again answers the same.
async fn lose_the_insert_race(f: &Fixture, outbox: &toolkit_db::outbox::OutboxHandle, max: &str) {
    let target = f.target(&Uuid::new_v4().to_string()).await;
    let (winner_snapshot, _) = verified("7", "1024");
    let winner = StoredSnapshot {
        content_digest: winner_snapshot.content_digest.clone(),
        ..f.snapshot(target.id, "7")
    };
    let (snapshot, plan) = verified("7", max);
    let loser = api_state(f.pg.db().await, outbox);
    let key = Uuid::new_v4().to_string();
    let claim = bss_products::infra::idempotency::IdempotencyClaimInput::new(
        format!(
            "/bss-products/v1/quota-adapter-targets/{}/discoveries",
            target.id
        ),
        key.clone(),
        bss_products::domain::idempotency::payload_digest(&serde_json::Value::Null).to_vec(),
        stored_now(),
        24,
    );
    let held = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let observer = f.pg.raw().await;
    let holder = {
        let (held, release) = (held.clone(), release.clone());
        let (scope, tenant, row) = (f.scope.clone(), f.tenant, winner.clone());
        let db = f.pg.db().await;
        async move {
            db.transaction_ref(move |tx| {
                Box::pin(async move {
                    intake::insert_snapshot(tx, &scope, tenant, &row)
                        .await
                        .unwrap();
                    held.notify_one();
                    release.notified().await;
                    Ok(())
                })
            })
            .await
            .unwrap();
        }
    };
    let staging = async {
        held.notified().await;
        let (staged, ()) = tokio::join!(
            stage_snapshot(
                &loser,
                &f.scope,
                (f.tenant, f.actor),
                &target,
                (&snapshot, &plan),
                Some(claim.clone()),
            ),
            async {
                wait_until_a_backend_blocks(&observer).await;
                release.notify_one();
            }
        );
        answered(staged).await
    };
    let ((), (status, body)) = tokio::join!(holder, staging);
    // Read before the key is sent again, which would record a receipt of its own.
    let receipts = receipts_of(f, &key).await;
    let again = stage_snapshot(
        &loser,
        &f.scope,
        (f.tenant, f.actor),
        &target,
        (&snapshot, &plan),
        Some(claim),
    )
    .await;
    assert_eq!(
        answered(again).await,
        (status, body.clone()),
        "the key answers the same"
    );
    if max == "1024" {
        assert_eq!(
            status, 200,
            "the loser answers the winner as a replay: {body}"
        );
        assert_eq!(body["snapshot"]["id"], json!(winner.id), "{body}");
        assert_eq!(receipts, [200], "the lost race records the key's receipt");
    } else {
        assert_eq!(status, 409, "{body}");
        assert!(body.to_string().contains("QUOTA_SOURCE_CONFLICT"), "{body}");
        assert!(
            receipts.is_empty(),
            "a refusal records no receipt: {receipts:?}"
        );
    }
    assert_eq!(snapshots_of(f, target.id).await, 1);
    observer.close().await.unwrap();
}

/// Two stagings of one revision on two pools, released together, through the staging the
/// discovery runs, store one snapshot: one creates it (201) and the other answers it as a replay
/// (200), with the same body. A staging that waits behind an uncommitted snapshot of the revision
/// answers it once it commits: as a replay when the digests agree, as a conflict when they do
/// not.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_concurrent_intakes_of_one_revision_store_one_snapshot_on_postgres() {
    let f = Fixture::new().await;
    let outbox = holding_outbox(&f).await;
    lose_the_insert_race(&f, &outbox, "1024").await;
    lose_the_insert_race(&f, &outbox, "2048").await;
    for revision in ["1", "2", "3"] {
        let target = f.target(&format!("pool-{revision}")).await;
        let (snapshot, plan) = verified(revision, "1024");
        let barrier = Arc::new(Barrier::new(2));
        let side = |state: Arc<ApiState>| {
            let (barrier, scope, target) = (barrier.clone(), f.scope.clone(), target.clone());
            let (snapshot, plan) = (snapshot.clone(), plan.clone());
            let (tenant, actor) = (f.tenant, f.actor);
            async move {
                barrier.wait().await;
                let staged = stage_snapshot(
                    &state,
                    &scope,
                    (tenant, actor),
                    &target,
                    (&snapshot, &plan),
                    None,
                )
                .await;
                answered(staged).await
            }
        };
        let (zero, one) = tokio::join!(
            side(api_state(f.pg.db().await, &outbox)),
            side(api_state(f.pg.db().await, &outbox))
        );
        let mut statuses = [zero.0, one.0];
        statuses.sort_unstable();
        assert_eq!(
            statuses,
            [200, 201],
            "one creates and one replays: {zero:?} {one:?}"
        );
        assert_eq!(zero.1, one.1, "revision {revision}");
        assert_eq!(zero.1["candidates"].as_array().map(Vec::len), Some(1));
        assert_eq!(snapshots_of(&f, target.id).await, 1);
    }
}

/// A manual record of the example, with `max` as its bound, for `target`.
fn manual_record(f: &Fixture, target: Uuid, max: &str) -> NewCandidate {
    let mut judged = judged_example();
    judged.raw["limits"]["max"] = json!(max);
    NewCandidate {
        target_id: target,
        origin: CandidateOrigin::Manual,
        snapshot_row_id: None,
        record_id: judged.record.id,
        capability_key: judged.record.capability_key.clone(),
        record_digest: record_digest(&judged.raw),
        record_json: judged.raw,
        evidence_json: json!({}),
        created_by: f.actor,
        created_at: f.now,
    }
}

/// A manual create whose insert waits behind another transaction's uncommitted manual candidate of
/// the same record id: the other commits, Postgres refuses the waiting insert and aborts its
/// transaction, and the create reads the winner afresh: its 200 for the same digest, a 409
/// `QUOTA_SOURCE_CONFLICT` for another.
async fn lose_the_manual_race(f: &Fixture, outbox: &toolkit_db::outbox::OutboxHandle, max: &str) {
    let target = f.target(&Uuid::new_v4().to_string()).await;
    let winner = manual_record(f, target.id, "1024");
    let loser = api_state(f.pg.db().await, outbox);
    let held = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let observer = f.pg.raw().await;
    let holder = {
        let (held, release) = (held.clone(), release.clone());
        let (scope, tenant, row) = (f.scope.clone(), f.tenant, winner.clone());
        let db = f.pg.db().await;
        async move {
            db.transaction_ref(move |tx| {
                Box::pin(async move {
                    let stored = intake::insert_candidates(tx, &scope, tenant, &[row])
                        .await
                        .unwrap();
                    held.notify_one();
                    release.notified().await;
                    Ok(stored)
                })
            })
            .await
            .unwrap()
        }
    };
    let creating = async {
        held.notified().await;
        let new = manual_record(f, target.id, max);
        let (written, ()) = tokio::join!(write_manual(&loser, &f.scope, f.tenant, &new), async {
            wait_until_a_backend_blocks(&observer).await;
            release.notify_one();
        });
        written
    };
    let (stored, written) = tokio::join!(holder, creating);
    let (status, body) = match written {
        Ok(response) => (response.status(), response),
        Err(error) => {
            let response = axum::response::IntoResponse::into_response(error);
            (response.status(), response)
        }
    };
    let body = axum::body::to_bytes(body.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    if max == "1024" {
        assert_eq!(status, 200, "the loser answers the winner: {body}");
        assert_eq!(body["id"], json!(stored[0].id), "{body}");
    } else {
        assert_eq!(status, 409, "{body}");
        assert!(body.to_string().contains("QUOTA_SOURCE_CONFLICT"), "{body}");
    }
    let count =
        f.pg.raw()
            .await
            .query_one_raw(sql(&format!(
                "SELECT count(*) AS n FROM bss.products_quota_candidate WHERE target_id = '{}'",
                target.id
            )))
            .await
            .unwrap()
            .unwrap()
            .try_get::<i64>("", "n")
            .unwrap();
    assert_eq!(count, 1);
    observer.close().await.unwrap();
}

/// A manual record created while a concurrent create of the same record id holds its uncommitted
/// row answers that row once it commits, from a fresh transaction: Postgres aborts the losing
/// transaction, so nothing is read inside it.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_manual_record_that_loses_the_insert_race_answers_the_winner_on_postgres() {
    let f = Fixture::new().await;
    let outbox = holding_outbox(&f).await;
    lose_the_manual_race(&f, &outbox, "1024").await;
    lose_the_manual_race(&f, &outbox, "2048").await;
}

/// The package's example instances, by id.
fn package_instance(id: &str) -> serde_json::Value {
    let units: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../../docs/quotas/schemas/quota-metric-unit.instances.json"
    ))
    .unwrap();
    let policies: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../../docs/quotas/schemas/quota-policies.instances.json"
    ))
    .unwrap();
    units
        .into_iter()
        .chain(policies)
        .find(|instance| instance["id"] == id)
        .unwrap()
}

/// The package's example record.
fn package_record() -> serde_json::Value {
    let snapshot: serde_json::Value = serde_json::from_str(include_str!(
        "../../../docs/quotas/schemas/quota-capability-discovery.example.json"
    ))
    .unwrap();
    snapshot["items"][0].clone()
}

/// A manual candidate of the package's example record on a fresh target, submitted against a SKU
/// at version 1 with the record's own unit, as a submit stores it; its author and submitter are
/// the fixture's actor.
async fn submitted_candidate(f: &Fixture) -> QuotaCandidate {
    let target = f.target(&Uuid::new_v4().to_string()).await;
    submitted_candidate_on(f, target.id).await
}

/// [`submitted_candidate`] on `target`, under a fresh record id: every candidate of the example
/// record on one target names one capability key.
async fn submitted_candidate_on(f: &Fixture, target: Uuid) -> QuotaCandidate {
    use bss_products::domain::quota::evidence::Evidence;
    use bss_products::domain::quota::review::{
        AllowedUnit, Pinned, PinnedEntity, Proposal, proposal_json,
    };
    use bss_products_sdk::quota::catalog::QuotaBasis;
    use bss_products_sdk::quota::digest::content_digest;
    let conn = f.db.conn().unwrap();
    let code = format!("SKU-{}", &Uuid::new_v4().simple().to_string()[..12]);
    let sku = bss_products::infra::storage::repo::insert_sku(
        &conn,
        &f.scope,
        f.tenant,
        bss_products::domain::sku::NewSku {
            code: code.clone(),
            name: code,
            r#type: bss_products_sdk::models::SkuType::Recurring,
            category_id: None,
            description: String::new(),
            sellable: true,
            gl_code: None,
            tax_category: None,
            invoice_line_template: None,
            billing_timing: None,
            usage_type_ref: None,
            unit: None,
        },
        f.actor,
        f.now,
    )
    .await
    .unwrap();
    bss_products::infra::storage::repo::append_version(
        &conn,
        &f.scope,
        f.tenant,
        sku.id,
        1,
        f.now.date(),
        &bss_products_sdk::models::SkuContent::from(&sku),
        f.now,
    )
    .await
    .unwrap();
    let mut record = package_record();
    record["id"] = json!(Uuid::new_v4().to_string());
    let reference = |name: &str| record[name].as_str().unwrap().to_owned();
    let mut evidence = EvidenceSet::default();
    let mut pinned = Pinned::default();
    for id in [
        reference("metric_ref"),
        reference("unit_ref"),
        reference("accounting_policy_ref"),
        reference("scope_policy_ref"),
    ] {
        let content = package_instance(&id);
        let entity = Evidence {
            gts_id: id.clone(),
            uuid: gts::GtsId::try_new(&id).unwrap().to_uuid(),
            resource_version: 1,
            digest: content_digest(&content),
        };
        evidence.insert(entity.clone());
        pinned.insert(PinnedEntity {
            evidence: entity,
            content,
        });
    }
    let unit_ref = reference("unit_ref");
    let unit = pinned.get(&unit_ref).unwrap().clone();
    let proposal = Proposal {
        sku_id: sku.id,
        sku_version: v(1),
        definition_id: None,
        allowed_units: vec![AllowedUnit {
            unit: serde_json::from_value(unit.content).unwrap(),
            evidence: unit.evidence,
            min: "0".to_owned(),
            step: "1".to_owned(),
            max: Some("1024".to_owned()),
        }],
        supported_bases: vec![QuotaBasis::Fixed],
        profile_conditions: [("en".to_owned(), "Stopped VMs count.".to_owned())].into(),
    };
    let candidate = intake::insert_candidates(
        &conn,
        &f.scope,
        f.tenant,
        &[NewCandidate {
            target_id: target,
            origin: CandidateOrigin::Manual,
            snapshot_row_id: None,
            record_id: Uuid::parse_str(record["id"].as_str().unwrap()).unwrap(),
            capability_key: reference("capability_key"),
            record_digest: record_digest(&record),
            record_json: record,
            evidence_json: evidence.to_json(),
            created_by: f.actor,
            created_at: f.now,
        }],
    )
    .await
    .unwrap()
    .remove(0);
    let moved = intake::review_candidate(
        &conn,
        &f.scope,
        f.tenant,
        candidate.id,
        1,
        ReviewWrite::Submit {
            proposal: proposal_json(&proposal),
            evidence: pinned.to_json(),
            by: f.actor,
            at: f.now,
            note: None,
        },
    )
    .await
    .unwrap();
    assert!(moved);
    intake::find_candidate(&conn, &f.scope, f.tenant, candidate.id)
        .await
        .unwrap()
        .unwrap()
}

/// A quota state over its own pool whose approvals wait at `hook` just before they move the
/// candidate.
async fn quota_state(
    f: &Fixture,
    outbox: &toolkit_db::outbox::OutboxHandle,
    hook: Arc<Barrier>,
) -> bss_products::api::rest::quota::QuotaState {
    let hub = Arc::new(toolkit::ClientHub::new());
    bss_products::api::rest::quota::QuotaState {
        api: api_state(f.pg.db().await, outbox),
        registry: Arc::new(
            bss_products::infra::quota::registry::TypesRegistryQuotaRegistry::new(hub.clone()),
        ),
        discovery: Arc::new(bss_products::infra::quota::discovery::HubQuotaDiscovery::new(hub)),
        settings: bss_products::config::ProductsConfig::default().quota_settings(),
        approve_hook: Some(hook),
    }
}

/// An independent approver's approval of `id` at version 2: its status and body.
async fn approve_once(
    state: &bss_products::api::rest::quota::QuotaState,
    tenant: Uuid,
    id: Uuid,
) -> (u16, serde_json::Value) {
    use bss_products::api::rest::quota::review::approve_candidate;
    let scope = AccessScope::for_tenant(tenant);
    let response = match approve_candidate(state, &scope, (tenant, Uuid::new_v4()), id, 2).await {
        Ok(response) => response,
        Err(error) => axum::response::IntoResponse::into_response(error),
    };
    let status = response.status().as_u16();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

/// Two approvers approve one submitted candidate at once, each in its own transaction on its own
/// pool, released together just before they move it: exactly one publishes (200), the other is
/// 409 `QUOTA_CANDIDATE_STATE` or `STALE_REVISION`, and one definition version is stored. Run in
/// both start orders.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_concurrent_approvals_publish_one_version_on_postgres() {
    let f = Fixture::new().await;
    let outbox = holding_outbox(&f).await;
    for swapped in [false, true] {
        let candidate = submitted_candidate(&f).await;
        let hook = Arc::new(Barrier::new(2));
        let (one, two) = (
            quota_state(&f, &outbox, hook.clone()).await,
            quota_state(&f, &outbox, hook).await,
        );
        let (a, b) = if swapped {
            let (b, a) = tokio::join!(
                approve_once(&two, f.tenant, candidate.id),
                approve_once(&one, f.tenant, candidate.id)
            );
            (a, b)
        } else {
            tokio::join!(
                approve_once(&one, f.tenant, candidate.id),
                approve_once(&two, f.tenant, candidate.id)
            )
        };
        let mut statuses = [a.0, b.0];
        statuses.sort_unstable();
        assert_eq!(statuses, [200, 409], "swapped {swapped}: {a:?} {b:?}");
        let refused = if a.0 == 409 { &a.1 } else { &b.1 };
        let code = refused.to_string();
        assert!(
            code.contains("QUOTA_CANDIDATE_STATE") || code.contains("STALE_REVISION"),
            "{refused}"
        );
        let count =
            f.pg.raw()
                .await
                .query_one_raw(sql(&format!(
                    "SELECT count(*) AS n FROM bss.products_quota_definition_version \
                 WHERE candidate_id = '{}'",
                    candidate.id
                )))
                .await
                .unwrap()
                .unwrap()
                .try_get::<i64>("", "n")
                .unwrap();
        assert_eq!(count, 1, "swapped {swapped}");
    }
}

/// The mapping versions stored for `target`.
async fn mappings_of(f: &Fixture, target: Uuid) -> i64 {
    f.pg.raw()
        .await
        .query_one_raw(sql(&format!(
            "SELECT count(*) AS n FROM bss.products_quota_mapping_version \
             WHERE target_id = '{target}'"
        )))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap()
}

/// Two submitted candidates of one capability key on one target, approved at once on two pools
/// and released together just before they move their candidates, race for the lineage's first
/// mapping version (P-D-270): one publishes (200), the other is 409 `CONTENDED` without a retry
/// and writes nothing, and the target holds one mapping row. The loser's resend then continues
/// the winner's lineage.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_first_approvals_of_one_lineage_store_one_mapping_on_postgres() {
    let f = Fixture::new().await;
    let outbox = holding_outbox(&f).await;
    let target = f.target(&Uuid::new_v4().to_string()).await;
    let first = submitted_candidate_on(&f, target.id).await;
    let second = submitted_candidate_on(&f, target.id).await;
    assert_eq!(first.capability_key, second.capability_key);
    let hook = Arc::new(Barrier::new(2));
    let (one, two) = (
        quota_state(&f, &outbox, hook.clone()).await,
        quota_state(&f, &outbox, hook).await,
    );
    let (a, b) = tokio::join!(
        approve_once(&one, f.tenant, first.id),
        approve_once(&two, f.tenant, second.id)
    );
    let mut statuses = [a.0, b.0];
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 409], "{a:?} {b:?}");
    let (won, lost, loser) = if a.0 == 200 {
        (&a.1, &b.1, second.id)
    } else {
        (&b.1, &a.1, first.id)
    };
    assert!(lost.to_string().contains("CONTENDED"), "{lost}");
    assert_eq!(mappings_of(&f, target.id).await, 1);
    let decided = |id: Uuid| {
        format!(
            "SELECT count(*) AS n FROM bss.products_quota_definition_version \
             WHERE candidate_id = '{id}'"
        )
    };
    let raw = f.pg.raw().await;
    let stored = raw
        .query_one_raw(sql(&decided(loser)))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap();
    assert_eq!(stored, 0, "the loser wrote nothing");
    raw.close().await.unwrap();
    // The loser sends its approval again: it continues the winner's lineage.
    let resend = quota_state(&f, &outbox, Arc::new(Barrier::new(1))).await;
    let (status, body) = approve_once(&resend, f.tenant, loser).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["definition"]["mapping_ref"]["id"], won["definition"]["mapping_ref"]["id"],
        "one lineage: {body}"
    );
    assert_eq!(mappings_of(&f, target.id).await, 2);
}

/// A PDP that grants every request of `tenant`'s subjects under `owner_tenant_id = tenant`.
struct TenantPdp(Uuid);

#[async_trait::async_trait]
impl authz_resolver_sdk::AuthZResolverApi for TenantPdp {
    async fn evaluate(
        &self,
        _ctx: toolkit_security::PlatformSecurityContext,
        _request: authz_resolver_sdk::models::EvaluationRequest,
    ) -> Result<
        authz_resolver_sdk::models::EvaluationResponse,
        toolkit_canonical_errors::CanonicalError,
    > {
        use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
        use authz_resolver_sdk::models::{EvaluationResponse, EvaluationResponseContext};
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        toolkit_security::pep_properties::OWNER_TENANT_ID,
                        vec![self.0],
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
}

/// A body holding U+0000 is 400 `VALIDATION` on its path before Postgres sees it (P-D-273):
/// Postgres refuses the character in text and `jsonb`, so a reject's note holding it would be a
/// 500 at the write. The candidate stays submitted.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_nul_in_a_review_body_is_validation_on_postgres() {
    use tower::ServiceExt;
    let f = Fixture::new().await;
    let outbox = holding_outbox(&f).await;
    let candidate = submitted_candidate(&f).await;
    let state = Arc::new(quota_state(&f, &outbox, Arc::new(Barrier::new(1))).await);
    let app =
        bss_products::api::rest::quota::router(state, &toolkit::api::OpenApiRegistryImpl::new())
            .layer(axum::Extension(authz_resolver_sdk::PolicyEnforcer::new(
                Arc::new(TenantPdp(f.tenant)),
            )));
    let approver = toolkit_security::SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(f.tenant)
        .subject_type("gts.cf.core.security.subject_user.v1~")
        .token_scopes(vec!["*".to_owned()])
        .build()
        .unwrap();
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method(axum::http::Method::POST)
                .uri(format!(
                    "/bss-products/v1/quota-candidates/{}/reject",
                    candidate.id
                ))
                .extension(approver)
                .header("Content-Type", "application/json")
                .header("If-Match", "\"2\"")
                .body(axum::body::Body::from(r#"{"note": "no\u0000"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = answered(Ok::<_, axum::response::Response>(response)).await;
    assert_eq!(status, 400, "{body}");
    assert!(body.to_string().contains("VALIDATION"), "{body}");
    assert_eq!(
        body["context"]["violations"][0]["subject"], "note",
        "{body}"
    );
    let conn = f.db.conn().unwrap();
    let stored = intake::find_candidate(&conn, &f.scope, f.tenant, candidate.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.state, CandidateState::Submitted);
}
