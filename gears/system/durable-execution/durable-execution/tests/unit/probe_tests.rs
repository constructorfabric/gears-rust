use super::*;
use crate::domain::journal::Journal;
use crate::infra::storage::{
    attempt, epoch,
    repository::tests::{definition, journal, store},
};
use sea_orm::{ActiveValue::Set, sea_query::Expr};
use std::time::Duration;
use toolkit_db::secure::{SecureDeleteExt, SecureUpdateExt, secure_insert};
use toolkit_security::{ScopeConstraint, ScopeFilter, pep_properties};
use uuid::Uuid;

async fn claimed(parallel: bool) -> (JournalStore, Journal, Claim, DateTime<Utc>) {
    let store = store().await;
    let mut definition = definition();
    if parallel {
        definition.parallel_groups = vec![
            definition
                .activities
                .iter()
                .map(|step| step.id.clone())
                .collect(),
        ];
    }
    let seed = journal();
    // Sub-microsecond precision must survive PostgreSQL timestamp rounding.
    let now = DateTime::from_timestamp(1_700_000_000, 123_456_789).unwrap();
    let mut value = Journal::new(
        seed.run.id,
        seed.run.owner,
        &definition.contract(),
        serde_json::Value::Null,
        now,
    )
    .unwrap();
    store
        .insert(AccessScope::allow_all(), value.clone())
        .await
        .unwrap();
    let claim = value
        .claim(&definition.contract(), 0, now, Duration::from_secs(120))
        .unwrap()
        .unwrap();
    store
        .save(AccessScope::allow_all(), 0, value, false)
        .await
        .unwrap();
    let value = store
        .get(&AccessScope::allow_all(), seed.run.id)
        .await
        .unwrap()
        .unwrap();
    (store, value, claim, now)
}

#[tokio::test]
async fn claim_probe_matches_sequential_and_parallel_ownership_and_exact_lease() {
    for parallel in [false, true] {
        let (store, mut value, claim, now) = claimed(parallel).await;
        let id = value.run.id;
        let scope = AccessScope::allow_all();
        let probe = store.claim_probe(&scope, id, claim).await.unwrap().unwrap();
        let until = now + chrono::Duration::seconds(120);
        assert_eq!(probe.definition, value.run.definition);
        assert_eq!(probe.registration_generation, value.registration_generation);
        assert_eq!(probe.lease_until, Some(until));
        assert!(!probe.cancellation_requested);
        for at in [now, until - chrono::Duration::nanoseconds(1), until] {
            assert_eq!(probe.owns(claim, at), value.owns(claim, at));
        }
        let stale = Claim {
            fence: claim.fence + 1,
            ..claim
        };
        assert!(!probe.owns(stale, now));
        let different_step = Claim {
            step: claim.step + 1,
            ..claim
        };
        assert!(!probe.owns(different_step, now));
        let outside = Claim { step: 128, ..claim };
        assert!(
            !store
                .claim_probe(&scope, id, outside)
                .await
                .unwrap()
                .unwrap()
                .owns(outside, now)
        );

        value
            .request_cancel_at(0, now, Some("probe cancellation"))
            .unwrap();
        store
            .save(scope.clone(), value.revision, value, false)
            .await
            .unwrap();
        let value = store.get(&scope, id).await.unwrap().unwrap();
        let probe = store.claim_probe(&scope, id, claim).await.unwrap().unwrap();
        assert!(probe.cancellation_requested);
        assert!(probe.owns(claim, now));
        assert_eq!(probe.owns(claim, now), value.owns(claim, now));
    }
}

#[tokio::test]
async fn claim_probe_observes_fence_revocation_and_fresh_scope_constraints() {
    for parallel in [false, true] {
        let (store, mut value, claim, now) = claimed(parallel).await;
        let id = value.run.id;
        let scope = AccessScope::allow_all();
        assert!(
            store
                .claim_probe(&scope, id, claim)
                .await
                .unwrap()
                .unwrap()
                .owns(claim, now)
        );
        for property in [
            pep_properties::OWNER_TENANT_ID,
            pep_properties::OWNER_ID,
            pep_properties::RESOURCE_ID,
        ] {
            let denied = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::r#in(
                property,
                vec![Uuid::new_v4().into()],
            )]));
            assert!(
                store
                    .claim_probe(&denied, id, claim)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        assert!(
            store
                .claim_probe(&scope, RunId(Uuid::new_v4()), claim)
                .await
                .unwrap()
                .is_none()
        );
        if let Some(stages) = &mut value.parallel {
            stages.steps[claim.step].fence += 1;
        } else {
            value.fence += 1;
        }
        store
            .save(scope.clone(), value.revision, value, false)
            .await
            .unwrap();
        let revoked = store.claim_probe(&scope, id, claim).await.unwrap().unwrap();
        assert!(!revoked.owns(claim, now));
    }
}

#[tokio::test]
async fn claim_probe_reads_only_selected_activity_and_never_hydrates_history() {
    let (store, value, claim, now) = claimed(true).await;
    let scope = AccessScope::allow_all();
    let conn = store.db.conn().unwrap();
    attempt::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .filter(Condition::all().add(attempt::Column::RunId.eq(value.run.id.0)))
        .col_expr(
            attempt::Column::State,
            Expr::value(b"invalid attempt history".to_vec()),
        )
        .exec(&conn)
        .await
        .unwrap();
    secure_insert::<epoch::Entity>(
        epoch::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(value.run.owner.tenant_id),
            owner_id: Set(value.run.owner.subject_id),
            run_id: Set(value.run.id.0),
            epoch: Set(17),
            state: Set(b"invalid archived history".to_vec()),
        },
        &scope,
        &conn,
    )
    .await
    .unwrap();
    activity::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .filter(
            Condition::all()
                .add(activity::Column::RunId.eq(value.run.id.0))
                .add(activity::Column::Position.ne(i32::try_from(claim.step).unwrap())),
        )
        .col_expr(
            activity::Column::State,
            Expr::value(b"invalid sibling checkpoint".to_vec()),
        )
        .exec(&conn)
        .await
        .unwrap();
    assert!(store.get(&scope, value.run.id).await.is_err());
    let probe = store
        .claim_probe(&scope, value.run.id, claim)
        .await
        .unwrap()
        .unwrap();
    assert!(probe.owns(claim, now));
    assert_eq!(
        probe.lease_until,
        Some(now + chrono::Duration::seconds(120))
    );
}

#[tokio::test]
async fn claim_probe_supports_legacy_blobs_without_normalized_rows() {
    for parallel in [false, true] {
        let (store, value, claim, now) = claimed(parallel).await;
        let scope = AccessScope::allow_all();
        let conn = store.db.conn().unwrap();
        run::Entity::update_many()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(run::Column::Id.eq(value.run.id.0)))
            .col_expr(run::Column::StorageVersion, Expr::value(0))
            .col_expr(run::Column::ActivityCount, Expr::value(0))
            .col_expr(
                run::Column::Journal,
                Expr::value(serde_json::to_vec(&value).unwrap()),
            )
            .exec(&conn)
            .await
            .unwrap();
        activity::Entity::delete_many()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(activity::Column::RunId.eq(value.run.id.0)))
            .exec(&conn)
            .await
            .unwrap();
        let probe = store
            .claim_probe(&scope, value.run.id, claim)
            .await
            .unwrap()
            .unwrap();
        assert!(probe.owns(claim, now));
        assert_eq!(probe.definition, value.run.definition);
        assert_eq!(
            probe.lease_until,
            Some(now + chrono::Duration::seconds(120))
        );
    }
}

#[tokio::test]
async fn claim_probe_rejects_a_header_read_before_a_committed_transition() {
    let (store, mut value, claim, now) = claimed(true).await;
    let scope = AccessScope::allow_all();
    let conn = store.db.conn().unwrap();
    let stale = read_header(&conn, &scope, value.run.id)
        .await
        .unwrap()
        .unwrap();
    let id = value.run.id;
    value.request_cancel_at(0, now, None).unwrap();
    store
        .save(scope.clone(), value.revision, value, false)
        .await
        .unwrap();
    assert!(matches!(
        read_claim(&conn, &scope, id, claim, stale).await,
        Err(StoreError::Conflict)
    ));
    let current = store.claim_probe(&scope, id, claim).await.unwrap().unwrap();
    assert!(current.cancellation_requested);
    assert!(current.owns(claim, now));
}

#[tokio::test]
async fn claim_probe_rejects_unsupported_storage_and_generation_mismatch() {
    for (column, number, expected) in [
        (
            run::Column::StorageVersion,
            2,
            "unsupported storage version",
        ),
        (
            run::Column::RegistrationGeneration,
            1,
            "registration generation mismatch",
        ),
    ] {
        let (store, value, claim, _) = claimed(false).await;
        let scope = AccessScope::allow_all();
        run::Entity::update_many()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(run::Column::Id.eq(value.run.id.0)))
            .col_expr(column, Expr::value(number))
            .exec(&store.db.conn().unwrap())
            .await
            .unwrap();
        assert!(
            matches!(store.claim_probe(&scope, value.run.id, claim).await, Err(StoreError::Invariant(message)) if message == expected)
        );
    }
}
