//! Unit tests for [`FakeMetadataRepo`].
//!
//! The fake is a thin `HashMap` wrapper around [`MetadataRepo`]; the
//! per-method semantics that matter end-to-end are exercised through
//! `service_tests.rs` against the same fake. Tests here pin behaviour
//! that is unique to the fake's storage role and would not surface
//! end-to-end:
//!
//! * Full insert / update / get / list / delete lifecycle round-trip
//!   (one combined test rather than one assertion per method).
//! * `delete_for_tenant` distinct-404 contract on a missing row.

use modkit_security::AccessScope;
use serde_json::json;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use modkit_odata::ODataQuery;

use crate::domain::error::DomainError;
use crate::domain::metadata::UpsertOutcome;
use crate::domain::metadata::repo::MetadataRepo;
use crate::domain::metadata::test_support::repo::FakeMetadataRepo;

fn scope() -> AccessScope {
    AccessScope::allow_all()
}

fn fixed_now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("epoch")
}

#[tokio::test]
async fn fake_repo_full_lifecycle_round_trip() {
    // Insert two rows for `tenant_a` out of `schema_uuid` order plus one
    // unrelated row for `tenant_b`. The single test covers:
    //   * insert path stamps `created_at == updated_at == now`
    //   * update path preserves `created_at` and advances `updated_at`
    //   * `get_for_tenant` returns `Ok(None)` for an absent row
    //   * `list_for_tenant` returns only the requested tenant's rows
    //     in stable `schema_uuid` order
    //   * `delete_for_tenant` removes the targeted row
    let repo = FakeMetadataRepo::new();
    let tenant_a = Uuid::from_u128(0x11);
    let tenant_b = Uuid::from_u128(0x22);
    // Ascending `schema_uuid` order: 0xA1 < 0xA2 < 0xA3.
    let schema_lo = Uuid::from_u128(0xA1);
    let schema_mid = Uuid::from_u128(0xA2);
    let schema_hi = Uuid::from_u128(0xA3);
    let t0 = fixed_now();

    // Insert out of order so the test pins the sort, not the seed.
    let inserted_hi = repo
        .upsert_for_tenant(&scope(), tenant_a, schema_hi, json!({"i": 3}), t0, None)
        .await
        .expect("insert hi");
    assert!(inserted_hi.was_inserted(), "first upsert must be Inserted");
    let inserted_hi_row = inserted_hi.into_row();
    assert_eq!(inserted_hi_row.created_at, t0);
    assert_eq!(inserted_hi_row.updated_at, t0);

    repo.upsert_for_tenant(&scope(), tenant_a, schema_lo, json!({"i": 1}), t0, None)
        .await
        .expect("insert lo");

    // Unrelated tenant's row — MUST not surface in tenant_a's list.
    repo.upsert_for_tenant(&scope(), tenant_b, schema_lo, json!({"i": 99}), t0, None)
        .await
        .expect("insert b/lo");

    // Update the `hi` row: `created_at` preserved, `updated_at` advances.
    let t1 = t0 + Duration::minutes(5);
    let updated = repo
        .upsert_for_tenant(&scope(), tenant_a, schema_hi, json!({"i": 4}), t1, None)
        .await
        .expect("update hi");
    let updated_row = match updated {
        UpsertOutcome::Updated(row) => row,
        other @ UpsertOutcome::Inserted(_) => panic!("expected Updated, got {other:?}"),
    };
    assert_eq!(updated_row.created_at, inserted_hi_row.created_at);
    assert_eq!(updated_row.updated_at, t1);
    assert_eq!(updated_row.value, json!({"i": 4}));

    // Missing row returns `Ok(None)`.
    let absent = repo
        .get_for_tenant(&scope(), tenant_a, schema_mid)
        .await
        .expect("get absent");
    assert!(absent.is_none(), "missing row must surface as Ok(None)");

    // List returns only tenant_a's rows in `schema_uuid` order. The
    // fake honours `query.limit` only; a default-construct returns
    // the full slice up to its built-in `50` cap, which comfortably
    // exceeds the seeded fixture.
    let page = repo
        .list_for_tenant(&scope(), tenant_a, &ODataQuery::default())
        .await
        .expect("list");
    let schemas: Vec<Uuid> = page.items.iter().map(|r| r.schema_uuid).collect();
    assert_eq!(schemas, vec![schema_lo, schema_hi]);
    assert!(page.items.iter().all(|r| r.tenant_id == tenant_a));

    // Delete the `lo` row; `get` confirms it is gone; tenant_b survives.
    repo.delete_for_tenant(&scope(), tenant_a, schema_lo)
        .await
        .expect("delete lo");
    let after = repo
        .get_for_tenant(&scope(), tenant_a, schema_lo)
        .await
        .expect("get lo after");
    assert!(after.is_none(), "row must be gone after delete");
    let b_row = repo
        .get_for_tenant(&scope(), tenant_b, schema_lo)
        .await
        .expect("get b/lo")
        .expect("tenant_b row must remain");
    assert_eq!(b_row.tenant_id, tenant_b);
}

#[tokio::test]
async fn delete_for_tenant_returns_metadata_entry_not_found_on_missing() {
    // Pin the distinct-404 contract — `DomainError::MetadataEntryNotFound`
    // (not a generic NotFound) so the REST layer can produce
    // `code=metadata_entry_not_found` distinct from
    // `metadata_schema_not_registered`.
    let repo = FakeMetadataRepo::new();
    let tenant = Uuid::from_u128(0x11);
    let schema = Uuid::from_u128(0xAA);
    let err = repo
        .delete_for_tenant(&scope(), tenant, schema)
        .await
        .expect_err("delete must fail on missing row");
    match err {
        DomainError::MetadataEntryNotFound { entry, .. } => {
            assert!(entry.contains(&tenant.to_string()));
            assert!(entry.contains(&schema.to_string()));
        }
        other => panic!("expected MetadataEntryNotFound, got {other:?}"),
    }
}
