//! Cross-dialect checks for the segment materialization.

use super::{down_statements, up_statements};

const BACKENDS: [sea_orm::DatabaseBackend; 3] = [
    sea_orm::DatabaseBackend::Postgres,
    sea_orm::DatabaseBackend::Sqlite,
    sea_orm::DatabaseBackend::MySql,
];

/// Every backend adds a checked `chain_depth`, the three entity indexes, and the
/// segment table with its cascade and lookup index.
#[test]
fn up_creates_the_same_objects_on_every_backend() {
    for backend in BACKENDS {
        let sql = up_statements(backend)
            .expect("supported backend")
            .join("\n");
        for needle in [
            "ADD COLUMN chain_depth",
            "ck_tr_entity_chain_depth",
            "idx_tr_entity_depth",
            "idx_tr_entity_kind_lifecycle",
            "(entity_kind, lifecycle_status, gts_id)",
            "idx_tr_entity_lifecycle",
            "(lifecycle_status, gts_id)",
            "CREATE TABLE IF NOT EXISTS types_registry__entity_gts_segment",
            "PRIMARY KEY (entity_id, segment_no)",
            "REFERENCES types_registry__entity (id) ON DELETE CASCADE",
            "CHECK (segment_no >= 0)",
            "idx_tr_entity_gts_segment_lookup",
            "segment_no, segment_name, major, is_type, minor, entity_id",
        ] {
            assert!(sql.contains(needle), "{backend:?} lacks {needle}");
        }
    }
}

/// `segment_name` is compared by byte ranges, so its collation must be binary.
#[test]
fn segment_names_use_binary_collation() {
    for (backend, collation) in [
        (sea_orm::DatabaseBackend::Postgres, "COLLATE \"C\""),
        (sea_orm::DatabaseBackend::Sqlite, "COLLATE BINARY"),
        (sea_orm::DatabaseBackend::MySql, "COLLATE ascii_bin"),
    ] {
        let sql = up_statements(backend)
            .expect("supported backend")
            .join("\n");
        let line = sql
            .lines()
            .find(|line| line.trim_start().starts_with("segment_name"))
            .expect("segment_name column");
        assert!(line.contains(collation), "{backend:?}: {line}");
    }
}

/// Down removes the table before the entity column it references.
#[test]
fn down_drops_the_table_then_the_column() {
    for backend in BACKENDS {
        let got = down_statements(backend).expect("supported backend");
        assert!(got[0].contains("DROP TABLE IF EXISTS types_registry__entity_gts_segment"));
        assert!(
            got.last()
                .expect("statement")
                .contains("DROP COLUMN chain_depth"),
            "{backend:?}"
        );
    }
}
