//! S2-06: the sealed transactional audit writer on real PostgreSQL through the restricted
//! runtime role, read back and verified through the read-only verifier role.
use super::*;
use crate::domain::audit::{
    ActorIdentities, AdminAttribute, AdminChange, AdminField, AdminTextKey, AttemptEvidence,
    AuditRow, AuditTrigger, ChainVerifier, ForceRequestObservation, Principal, SealedAudit,
    ServiceRole, VerifyError, verify_entry,
};
use crate::infra::storage::repo::TransactionRunner;
use crate::infra::storage::repo::audit::{
    self as writer, AuditStoreError, committed_chain_page, from_model, order_facts,
};
use crate::infra::storage::scoped::PrivateScope;
use bss_orders_lifecycle_sdk::catalog::{Reason, Trigger};
use bss_orders_lifecycle_sdk::models::{DelegationProofRef, IdempotencyKey};
use sea_orm::ConnectionTrait;
use toolkit_security::{AccessScope, SecurityContext};

pub(super) fn identities() -> ActorIdentities {
    ActorIdentities::new(
        Some(Principal {
            subject_id: u(900),
            subject_tenant_id: u(901),
        }),
        [(
            ServiceRole::Workflow,
            Principal {
                subject_id: u(902),
                subject_tenant_id: u(901),
            },
        )],
    )
    .unwrap()
}
pub(super) fn ctx(subject: u128) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(u(subject))
        .subject_tenant_id(u(10))
        .subject_type("user")
        .build()
        .unwrap()
}
fn caller(subject: u128) -> crate::authz::Caller {
    crate::authz::Caller::new(ctx(subject), None)
}
pub(super) fn evidence_at(
    subject: u128,
    at: time::OffsetDateTime,
    proof: Option<&str>,
) -> AttemptEvidence {
    let proof = proof.map(|p| DelegationProofRef::try_from(p.to_owned()).unwrap());
    AttemptEvidence::new(
        identities().classify(&ctx(subject)).unwrap(),
        proof.as_ref(),
        &IdempotencyKey::try_from(format!("key-{subject}")).unwrap(),
        Some(u(77)),
        at,
    )
    .unwrap()
}
pub(super) fn evidence(subject: u128) -> AttemptEvidence {
    // Sub-microsecond residue proves normalization happens once, before hashing and storage.
    evidence_at(
        subject,
        now() + time::Duration::nanoseconds(123_456_789),
        None,
    )
}
fn scope(id: u128) -> AccessScope {
    AccessScope::for_resources(vec![u(id)])
}
async fn lock<T: TransactionRunner>(tx: &T, id: u128) -> anyhow::Result<repo::LockedOrder<'_, T>> {
    // Lock the current row (the engine's authorized lock), never a stale prefetched copy.
    let locked = repo::LockedOrder::lock_current(tx, &scope(id), u(id)).await?;
    locked.ok_or_else(|| anyhow::anyhow!("order {id} not visible"))
}
/// Committed create: aggregate at `audit_sequence` 0, empty version 1, sealed sequence-1 entry.
async fn create_audited(db: &Db, id: u128) -> anyhow::Result<SealedAudit> {
    db.transaction_ref_mapped(move |tx| {
        Box::pin(async move {
            let row = repo::insert_order(tx, &scope(id), order(id)).await?;
            let mut locked = repo::LockedOrder::acquire(tx, &scope(id), &row).await?;
            locked.insert_order_version(version(id, 1, None)).await?;
            let facts = locked.audit_facts()?;
            let pending = evidence(40).committed_create(Uuid::new_v4(), &facts)?;
            anyhow::Ok(writer::append_committed(&mut locked, pending).await?)
        })
    })
    .await
}
async fn mutate_in<T: TransactionRunner>(tx: &T, id: u128) -> anyhow::Result<SealedAudit> {
    let mut locked = lock(tx, id).await?;
    let mut row = locked.row().clone();
    row.draft_revision += 1;
    let before = order_facts(locked.row())?;
    locked.replace(&scope(id), row).await?;
    let after = locked.audit_facts()?;
    let pending = evidence(40).committed_transition(
        Uuid::new_v4(),
        AuditTrigger::Public(Trigger::DraftMutate),
        &before,
        &after,
        None,
    )?;
    anyhow::Ok(writer::append_committed(&mut locked, pending).await?)
}
async fn mutate(db: &Db, id: u128) -> anyhow::Result<SealedAudit> {
    db.transaction_ref_mapped(move |tx| Box::pin(async move { mutate_in(tx, id).await }))
        .await
}
/// Read the committed chain through the verifier role and verify it against the counter.
async fn verify_stored(
    pg: &Pg,
    verifier: &Db,
    id: u128,
) -> anyhow::Result<Result<i64, VerifyError>> {
    let rows = verifier
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                anyhow::Ok(committed_chain_page(tx, &scope(id), u(id), 0, 500).await?)
            })
        })
        .await?;
    let counter = pg
        .scalar(&format!(
            "SELECT audit_sequence AS n FROM bss_orders__order WHERE order_id='{}'",
            u(id)
        ))
        .await?;
    let mut chain = ChainVerifier::new(u(10), u(id));
    for (row, hash) in &rows {
        if let Err(e) = chain.push(row, hash) {
            return anyhow::Ok(Err(e));
        }
    }
    anyhow::Ok(chain.finish(counter).map(|h| h.map_or(0, |h| h.sequence)))
}
async fn stored(pg: &Pg, audit_id: Uuid) -> anyhow::Result<(AuditRow, Vec<u8>)> {
    // Every row these tests read back was appended for subject tenant 10.
    let scope = PrivateScope::for_refusal(&caller(40));
    let model =
        repo::private::find_transition_audit(&pg.db.conn()?, scope.access_scope(), audit_id)
            .await?
            .unwrap();
    anyhow::Ok(from_model(&model)?)
}
/// SQL copying one stored row under a fresh audit identity, optionally overriding columns.
fn copy_row(filter: &str, overrides: &str) -> String {
    format!(
        "INSERT INTO bss_orders__transition_audit SELECT (jsonb_populate_record(NULL::bss_orders__transition_audit, \
         to_jsonb(t) || jsonb_build_object('audit_id', gen_random_uuid(){overrides}))).* \
         FROM bss_orders__transition_audit t WHERE {filter}"
    )
}
async fn await_lock_wait(pg: &Pg) -> anyhow::Result<()> {
    for _ in 0..200 {
        if pg
            .scalar("SELECT count(*) AS n FROM pg_stat_activity WHERE wait_event_type='Lock'")
            .await?
            > 0
        {
            return anyhow::Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    anyhow::bail!("the contender never blocked on the aggregate lock")
}

#[tokio::test]
async fn sealed_chain_round_trips_through_postgres_and_verifies_from_stored_bytes()
-> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (runtime, _) = pg.role("runtime").await?;
    let (verifier, _) = pg.role("verifier").await?;
    let create = create_audited(&runtime, 1).await?;
    assert_eq!(create.row().sequence, Some(1));
    assert_eq!(
        create.row().prev_hash.as_deref(),
        Some(crate::domain::audit::genesis(u(10), u(1)).as_slice())
    );
    let second = mutate(&runtime, 1).await?;
    assert_eq!(
        second.row().prev_hash.as_deref(),
        Some(create.entry_hash().as_slice())
    );
    // Administrative edit: one entry per changed field at consecutive sequences.
    let admin = runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let mut locked = lock(tx, 1).await?;
                let facts = locked.audit_facts()?;
                let changes = [
                    AdminChange {
                        field: AdminField::Order(AdminAttribute::ExternalReference),
                        prior: None,
                        new: Some("PO-1".into()),
                    },
                    AdminChange {
                        field: AdminField::Line(u(5), AdminAttribute::InternalNotes),
                        prior: Some("a".into()),
                        new: Some("A".into()),
                    },
                    // Whitespace-only change: still distinct after keyed minimization, so the
                    // `prior_value IS DISTINCT FROM new_value` CHECK admits it (D-204).
                    AdminChange {
                        field: AdminField::Order(AdminAttribute::DisplayLabel),
                        prior: Some("Plaintext Label".into()),
                        new: Some("Plaintext Label ".into()),
                    },
                ];
                let key = AdminTextKey::new("pg-1", &[9; 32])?;
                let pending =
                    evidence(40).administrative_edits(&key, &facts, &changes, Uuid::new_v4)?;
                anyhow::Ok(writer::append_committed_all(&mut locked, pending).await?)
            })
        })
        .await?;
    assert_eq!(
        admin.iter().map(|s| s.row().sequence).collect::<Vec<_>>(),
        [Some(3), Some(4), Some(5)]
    );
    // Cancel with caller text: exact UTF-8 bytes survive storage.
    let cancel = runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let mut locked = lock(tx, 1).await?;
                let before = locked.audit_facts()?;
                let mut row = locked.row().clone();
                row.state = "cancelled".into();
                locked.replace(&scope(1), row).await?;
                let after = locked.audit_facts()?;
                let pending = evidence(40).committed_transition(
                    Uuid::new_v4(),
                    AuditTrigger::Public(Trigger::Cancel),
                    &before,
                    &after,
                    Some("Requested cancellation \u{2014} caf\u{e9}"),
                )?;
                anyhow::Ok(writer::append_committed(&mut locked, pending).await?)
            })
        })
        .await?;
    assert_eq!(verify_stored(&pg, &verifier, 1).await??, 6);
    // Every stored row re-verifies from its read-back bytes, including the microsecond instant.
    for sealed in [&create, &second, &admin[0], &admin[1], &admin[2], &cancel] {
        let (row, hash) = stored(&pg, sealed.row().audit_id).await?;
        assert_eq!(&row, sealed.row(), "storage round trip is exact");
        assert_eq!(hash, sealed.entry_hash().to_vec());
        verify_entry(&row, &hash)?;
        assert_eq!(
            time::OffsetDateTime::unix_timestamp_nanos(row.created_at) % 1_000,
            0
        );
    }
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__transition_audit WHERE created_at = timestamptz '2026-09-21 14:13:20.123456+00'").await?,
        6,
        "stored instant is the normalized microsecond value"
    );
    // D-204: keyed values only; no plaintext label or note in any stored column.
    assert_eq!(pg.scalar("SELECT count(*) AS n FROM bss_orders__transition_audit WHERE prior_value LIKE 'hmac-sha256:v1:pg-1:%' AND new_value LIKE 'hmac-sha256:v1:pg-1:%' AND (changed_field LIKE 'lines/%/internal_notes' OR changed_field='display_label')").await?, 2);
    assert_eq!(pg.scalar("SELECT count(*) AS n FROM bss_orders__transition_audit WHERE prior_value LIKE 'sha256:%' OR new_value LIKE 'sha256:%'").await?, 0);
    assert_eq!(pg.scalar("SELECT count(*) AS n FROM bss_orders__transition_audit t WHERE strpos(row_to_json(t)::text, 'Plaintext') > 0").await?, 0);
    anyhow::Ok(())
}

#[tokio::test]
async fn boundary_instants_and_null_empty_values_round_trip_and_verify() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (runtime, _) = pg.role("runtime").await?;
    let instants = [
        time::OffsetDateTime::from_unix_timestamp_nanos(-1_000).unwrap(), // 1969-12-31T23:59:59.999999Z
        time::OffsetDateTime::UNIX_EPOCH,
        time::macros::datetime!(0001-01-01 00:00:00 UTC),
        time::macros::datetime!(9999-12-31 23:59:59.999_999 UTC),
    ];
    let mut ids = Vec::new();
    for (n, at) in instants.into_iter().enumerate() {
        let proof = if n % 2 == 0 { Some("proof:x") } else { None };
        let sealed = evidence_at(40, at, proof).unresolved_refusal(
            Uuid::new_v4(),
            AuditTrigger::Public(Trigger::Submit),
            Some(u(999)),
            Reason::OrderNotFound,
        )?;
        ids.push((sealed.row().audit_id, sealed.entry_hash()));
        runtime
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    anyhow::Ok(
                        writer::append_unresolved_refusal(
                            tx,
                            &PrivateScope::for_refusal(&caller(40)),
                            sealed,
                        )
                        .await?,
                    )
                })
            })
            .await?;
    }
    for (id, hash) in ids {
        let (row, stored_hash) = stored(&pg, id).await?;
        assert_eq!(stored_hash, hash.to_vec());
        verify_entry(&row, &stored_hash)?;
    }
    assert_eq!(pg.scalar("SELECT count(*) AS n FROM bss_orders__transition_audit WHERE delegation_proof_ref IS NULL").await?, 2);
    anyhow::Ok(())
}

#[tokio::test]
async fn failed_audit_aborts_the_mutation_and_rollback_leaves_no_sequence_gap() -> anyhow::Result<()>
{
    let pg = Pg::new().await?;
    let (runtime, _) = pg.role("runtime").await?;
    let (verifier, _) = pg.role("verifier").await?;
    create_audited(&runtime, 1).await?;
    // (a) The audit is refused after the mutation was applied: the whole transition aborts.
    let refused = runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let mut locked = lock(tx, 1).await?;
                let before = locked.audit_facts()?;
                let mut row = locked.row().clone();
                row.state = "cancelled".into();
                locked.replace(&scope(1), row).await?;
                // Built against stale "after" facts: the writer refuses rather than audit a
                // state the aggregate does not hold.
                let pending = evidence(40).committed_transition(
                    Uuid::new_v4(),
                    AuditTrigger::Public(Trigger::DraftMutate),
                    &before,
                    &before,
                    None,
                )?;
                anyhow::Ok(writer::append_committed(&mut locked, pending).await?)
            })
        })
        .await;
    assert!(matches!(
        refused.unwrap_err().downcast::<AuditStoreError>(),
        Ok(AuditStoreError::Foreign)
    ));
    // (b) The audit row was inserted but the transaction fails afterwards: all rolls back.
    let injected: anyhow::Result<()> = runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let sealed = mutate_in(tx, 1).await?;
                assert_eq!(sealed.row().sequence, Some(2));
                anyhow::bail!("injected failure after the audit append");
            })
        })
        .await;
    assert!(injected.is_err());
    // (c) A database-level insert failure (a duplicate audit identity) aborts likewise.
    let existing_id = repo::private::resolved_audit_page(&pg.db.conn()?, &scope(1), u(1), None, 1)
        .await?[0]
        .audit_id;
    let duplicate = runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let mut locked = lock(tx, 1).await?;
                let before = locked.audit_facts()?;
                let mut row = locked.row().clone();
                row.draft_revision += 1;
                locked.replace(&scope(1), row).await?;
                let after = locked.audit_facts()?;
                let pending = evidence(40).committed_transition(
                    existing_id,
                    AuditTrigger::Public(Trigger::DraftMutate),
                    &before,
                    &after,
                    None,
                )?;
                anyhow::Ok(writer::append_committed(&mut locked, pending).await?)
            })
        })
        .await;
    assert!(matches!(
        duplicate.unwrap_err().downcast::<AuditStoreError>(),
        Ok(AuditStoreError::Store(_))
    ));
    let existing = pg
        .scalar("SELECT count(*) AS n FROM bss_orders__transition_audit")
        .await?;
    assert_eq!(existing, 1);
    assert_eq!(
        pg.scalar("SELECT audit_sequence AS n FROM bss_orders__order")
            .await?,
        1
    );
    assert_eq!(
        pg.scalar("SELECT draft_revision AS n FROM bss_orders__order")
            .await?,
        0,
        "no mutation survived"
    );
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__order WHERE state='draft'")
            .await?,
        1
    );
    // The next committed append takes sequence 2: the rolled-back attempts consumed nothing.
    let next = mutate(&runtime, 1).await?;
    assert_eq!(next.row().sequence, Some(2));
    assert_eq!(verify_stored(&pg, &verifier, 1).await??, 2);
    anyhow::Ok(())
}

#[tokio::test]
async fn concurrent_same_order_appends_serialize_on_the_aggregate_lock_without_forks()
-> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (runtime, _) = pg.role("runtime").await?;
    let (verifier, _) = pg.role("verifier").await?;
    create_audited(&runtime, 1).await?;
    let (held_tx, held_rx) = tokio::sync::oneshot::channel::<()>();
    let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
    let first_db = runtime.clone();
    let first = tokio::spawn(async move {
        first_db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let held = lock(tx, 1).await?;
                    held_tx.send(()).ok();
                    go_rx.await.ok();
                    drop(held);
                    mutate_in(tx, 1).await
                })
            })
            .await
    });
    held_rx.await?;
    let second_db = runtime.clone();
    let second = tokio::spawn(async move { mutate(&second_db, 1).await });
    await_lock_wait(&pg).await?;
    go_tx.send(()).ok();
    let a = first.await??;
    let b = second.await??;
    let mut sequences = [a.row().sequence.unwrap(), b.row().sequence.unwrap()];
    sequences.sort_unstable();
    assert_eq!(sequences, [2, 3], "contiguous, no duplicate position");
    assert_eq!(verify_stored(&pg, &verifier, 1).await??, 3);
    // Many concurrent appenders still form one linear chain.
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let db = runtime.clone();
            tokio::spawn(async move { mutate(&db, 1).await })
        })
        .collect();
    for task in tasks {
        task.await??;
    }
    assert_eq!(verify_stored(&pg, &verifier, 1).await??, 11);
    // Even a writer that bypassed the lock cannot fork the chain: (order_id, sequence) is unique.
    assert!(pg.sql(&copy_row("sequence=2", "")).await.is_err());
    anyhow::Ok(())
}

#[tokio::test]
async fn different_orders_never_share_a_chain_lock() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (runtime, _) = pg.role("runtime").await?;
    create_audited(&runtime, 1).await?;
    create_audited(&runtime, 2).await?;
    let (held_tx, held_rx) = tokio::sync::oneshot::channel::<()>();
    let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
    let holder_db = runtime.clone();
    let holder = tokio::spawn(async move {
        holder_db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let _locked = lock(tx, 1).await?;
                    held_tx.send(()).ok();
                    go_rx.await.ok();
                    anyhow::Ok(())
                })
            })
            .await
    });
    held_rx.await?;
    let other = tokio::time::timeout(std::time::Duration::from_secs(10), mutate(&runtime, 2)).await;
    assert_eq!(
        other??.row().sequence,
        Some(2),
        "order 2 appended while order 1 stayed locked"
    );
    go_tx.send(()).ok();
    holder.await??;
    anyhow::Ok(())
}

#[tokio::test]
async fn early_denial_appends_without_target_lookup_lock_or_enrichment() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (runtime, _) = pg.role("runtime").await?;
    create_audited(&runtime, 1).await?;
    // Another transaction holds the existing order's row lock for the whole denial.
    let (held_tx, held_rx) = tokio::sync::oneshot::channel::<()>();
    let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
    let holder_db = runtime.clone();
    let holder = tokio::spawn(async move {
        holder_db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let _locked = lock(tx, 1).await?;
                    held_tx.send(()).ok();
                    go_rx.await.ok();
                    anyhow::Ok(())
                })
            })
            .await
    });
    held_rx.await?;
    let mut appended = Vec::new();
    for target in [1, 4242] {
        // Existing-but-inaccessible and unknown identifiers produce the same row shape.
        let sealed = evidence(40).unresolved_refusal(
            Uuid::new_v4(),
            AuditTrigger::Public(Trigger::Cancel),
            Some(u(target)),
            Reason::OrderNotFound,
        )?;
        let db = runtime.clone();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            db.transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    anyhow::Ok(
                        writer::append_unresolved_refusal(
                            tx,
                            &PrivateScope::for_refusal(&caller(40)),
                            sealed,
                        )
                        .await?,
                    )
                })
            }),
        )
        .await;
        appended.push(result??);
    }
    go_tx.send(()).ok();
    holder.await??;
    for sealed in &appended {
        let (row, hash) = stored(&pg, sealed.row().audit_id).await?;
        verify_entry(&row, &hash)?;
        assert_eq!(
            (row.order_id, row.audit_tenant_id, row.resource_tenant_id),
            (None, None, None)
        );
        assert_eq!(
            (row.from_state, row.to_state, row.version, row.sequence),
            (None, None, None, None)
        );
        assert_eq!(
            row.subject_tenant_id,
            u(10),
            "trusted subject tenant, never target tenancy"
        );
    }
    assert_eq!(appended[0].row().requested_order_ref, Some(u(1)));
    assert_eq!(appended[1].row().requested_order_ref, Some(u(4242)));
    // The refusal scope is the authenticated subject tenant: another subject tenant's evidence
    // cannot be inserted under it, and resolved rows cannot use the unresolved path.
    let foreign = AttemptEvidence::new(
        identities().classify(
            &SecurityContext::builder()
                .subject_id(u(41))
                .subject_tenant_id(u(11))
                .build()?,
        )?,
        None,
        &IdempotencyKey::try_from("k".to_owned()).unwrap(),
        None,
        now(),
    )?
    .unresolved_refusal(
        Uuid::new_v4(),
        AuditTrigger::Public(Trigger::Cancel),
        Some(u(1)),
        Reason::OrderNotFound,
    )?;
    let denied = runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                anyhow::Ok(
                    writer::append_unresolved_refusal(
                        tx,
                        &PrivateScope::for_refusal(&caller(40)),
                        foreign,
                    )
                    .await?,
                )
            })
        })
        .await;
    assert!(denied.is_err());
    assert_eq!(
        pg.scalar("SELECT audit_sequence AS n FROM bss_orders__order")
            .await?,
        1,
        "refusals take no sequence"
    );
    anyhow::Ok(())
}

#[tokio::test]
async fn resolved_refusals_and_force_request_observations_are_stored_exactly() -> anyhow::Result<()>
{
    let pg = Pg::new().await?;
    let (runtime, _) = pg.role("runtime").await?;
    let (verifier, _) = pg.role("verifier").await?;
    create_audited(&runtime, 1).await?;
    mutate(&runtime, 1).await?;
    let (refusal, request) = runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let mut locked = lock(tx, 1).await?;
                let facts = locked.audit_facts()?;
                let refusal = evidence(40).resolved_refusal(
                    Uuid::new_v4(),
                    AuditTrigger::Public(Trigger::Submit),
                    &facts,
                    Reason::NotAdmissible,
                )?;
                let refusal = writer::append_resolved_refusal(&locked, refusal).await?;
                // Put the aggregate in fulfillment (engine-owned in production) and record a
                // forced-failure request, which observes the exact committed head under the lock.
                let mut row = locked.row().clone();
                row.state = "in_fulfillment".into();
                locked.replace(&scope(1), row).await?;
                let facts = locked.audit_facts()?;
                let request = evidence(40).resolved_refusal(
                    Uuid::new_v4(),
                    AuditTrigger::Public(Trigger::ForceFailUnreconciled),
                    &facts,
                    Reason::SecondApproverRequired,
                )?;
                let request = writer::append_resolved_refusal(&locked, request).await?;
                // A refusal built on stale facts is refused, never stored.
                let stale = evidence(40).resolved_refusal(
                    Uuid::new_v4(),
                    AuditTrigger::Public(Trigger::Submit),
                    &crate::domain::audit::OrderFacts {
                        version: 9,
                        ..facts
                    },
                    Reason::NotAdmissible,
                )?;
                assert!(matches!(
                    writer::append_resolved_refusal(&locked, stale).await,
                    Err(AuditStoreError::Foreign)
                ));
                anyhow::Ok((refusal, request))
            })
        })
        .await?;
    for sealed in [&refusal, &request] {
        let (row, hash) = stored(&pg, sealed.row().audit_id).await?;
        assert_eq!(&row, sealed.row());
        verify_entry(&row, &hash)?;
        assert_eq!((row.sequence, row.prev_hash), (None, None));
        assert_eq!(row.requested_order_ref, Some(u(1)));
    }
    assert_eq!(
        request.row().force_request_observation,
        Some(ForceRequestObservation {
            audit_sequence: 2,
            state: "in_fulfillment".into(),
            version: 1
        })
    );
    assert_eq!(pg.scalar("SELECT (force_request_observation->>'audit_sequence')::bigint AS n FROM bss_orders__transition_audit WHERE force_request_observation IS NOT NULL").await?, 2);
    // Refusals are outside the committed chain; the chain and counter are unchanged.
    assert_eq!(verify_stored(&pg, &verifier, 1).await??, 2);
    anyhow::Ok(())
}

#[tokio::test]
async fn identity_removal_and_privileged_rewrites_never_pass_verification() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (runtime, runtime_raw) = pg.role("runtime").await?;
    let (verifier, verifier_raw) = pg.role("verifier").await?;
    let (_, retention_raw) = pg.role("retention").await?;
    let (_, private_raw) = pg.role("private").await?;
    create_audited(&runtime, 1).await?;
    mutate(&runtime, 1).await?;
    // Identity removal happens in the identity platform; no Orders role may rewrite or remove
    // the opaque actor reference or its hashes.
    for conn in [&runtime_raw, &verifier_raw, &retention_raw, &private_raw] {
        assert!(conn.execute_unprepared("UPDATE bss_orders__transition_audit SET actor='00000000-0000-0000-0000-000000000000'").await.is_err());
        assert!(
            conn.execute_unprepared(
                "DELETE FROM bss_orders__transition_audit WHERE outcome='committed'"
            )
            .await
            .is_err()
        );
    }
    assert!(
        verifier_raw.execute_unprepared("INSERT INTO bss_orders__transition_audit SELECT * FROM bss_orders__transition_audit LIMIT 0").await.is_err(),
        "the verifier holds no INSERT grant"
    );
    // The verifier reproduces digests from stored bytes alone after the identity is gone.
    assert_eq!(verify_stored(&pg, &verifier, 1).await??, 2);
    // A privileged "erasure" rewrite (owner bypassing the trigger) is detected, never exempted.
    pg.sql("ALTER TABLE bss_orders__transition_audit DISABLE TRIGGER bss_orders__transition_audit_immutable; UPDATE bss_orders__transition_audit SET actor='00000000-0000-0000-0000-0000000000ee' WHERE sequence=1; ALTER TABLE bss_orders__transition_audit ENABLE TRIGGER bss_orders__transition_audit_immutable").await?;
    assert!(matches!(
        verify_stored(&pg, &verifier, 1).await?,
        Err(VerifyError::DigestMismatch { .. })
    ));
    anyhow::Ok(())
}

/// Migration-class SQL storing one frozen independent audit vector exactly as authored.
fn vector_row_sql(vector: &serde_json::Value) -> String {
    let input = &vector["input"];
    let text = |key: &str| {
        input[key].as_str().map_or("NULL".to_owned(), |value| {
            format!("'{}'", value.replace('\'', "''"))
        })
    };
    let number = |key: &str| input[key].as_str().map_or("NULL".to_owned(), str::to_owned);
    let bytes = |key: &str| {
        input[key]
            .as_str()
            .map_or("NULL".to_owned(), |hex| format!("'\\x{hex}'::bytea"))
    };
    let observation = input["force_request_observation"].as_object().map_or(
        "NULL".to_owned(),
        |object| {
            format!(
                "jsonb_build_object('audit_sequence', {}::bigint, 'state', '{}', 'version', {}::int)",
                object["audit_sequence"].as_str().unwrap(),
                object["state"].as_str().unwrap(),
                object["version"].as_str().unwrap()
            )
        },
    );
    let columns = [
        text("audit_id"),
        input["hash_version"].to_string(),
        text("audit_tenant_id"),
        text("subject_tenant_id"),
        text("resource_tenant_id"),
        text("order_id"),
        text("requested_order_ref"),
        number("sequence"),
        bytes("prev_hash"),
        format!("'\\x{}'::bytea", vector["sha256"].as_str().unwrap()),
        text("from_state"),
        text("to_state"),
        text("trigger"),
        text("outcome"),
        text("actor"),
        text("actor_class"),
        text("delegation_proof_ref"),
        text("reason"),
        text("caller_reason"),
        observation,
        text("changed_field"),
        text("prior_value"),
        text("new_value"),
        text("idempotency_key"),
        text("correlation_id"),
        number("version"),
        format!(
            "timestamptz 'epoch' + {} * interval '1 microsecond'",
            input["created_at"].as_str().unwrap()
        ),
    ];
    format!(
        "INSERT INTO bss_orders__transition_audit (audit_id,hash_version,audit_tenant_id,subject_tenant_id,resource_tenant_id,order_id,requested_order_ref,sequence,prev_hash,entry_hash,from_state,to_state,trigger,outcome,actor,actor_class,delegation_proof_ref,reason,caller_reason,force_request_observation,changed_field,prior_value,new_value,idempotency_key,correlation_id,version,created_at) VALUES ({})",
        columns.join(",")
    )
}
fn conformance_vectors() -> anyhow::Result<serde_json::Value> {
    anyhow::Ok(serde_json::from_str(include_str!(
        "../../../../tests/fixtures/conformance-v1.json"
    ))?)
}
fn conformance_vector(fixture: &serde_json::Value, id: &str) -> serde_json::Value {
    fixture["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == id)
        .unwrap()
        .clone()
}

#[tokio::test]
async fn historical_v1_v2_rows_verify_from_storage_and_new_writers_must_seal_v3()
-> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (runtime, runtime_raw) = pg.role("runtime").await?;
    let (verifier, _) = pg.role("verifier").await?;
    // Frozen independent vectors (order 0x64, namespace 0x0a) loaded by a migration-class
    // connection: v1 create then v2 cancel.
    let fixture = conformance_vectors()?;
    let vector = |id: &str| conformance_vector(&fixture, id);
    create(&pg.db, 0x64).await?;
    pg.sql("UPDATE bss_orders__order SET audit_sequence=2, state='cancelled' WHERE order_id='00000000-0000-0000-0000-000000000064'").await?;
    for id in ["v1-create", "v2-after-v1-caller-reason"] {
        pg.sql(&vector_row_sql(&vector(id))).await?;
    }
    assert_eq!(
        verify_stored(&pg, &verifier, 0x64).await??,
        2,
        "frozen v1/v2 chain verifies from storage"
    );
    // A new runtime writer cannot store v1/v2, even as a correctly digested historical copy.
    let copy = copy_row("sequence=2 AND hash_version=2", ", 'sequence', 3");
    assert!(runtime_raw.execute_unprepared(&copy).await.is_err());
    // The same copy is storable by a migration-class connection: only the writer rule refused.
    assert!(pg.sql(&format!("BEGIN; {copy}; ROLLBACK")).await.is_ok());
    // The sealed writer extends the historical chain with v3, linked to the stored v2 digest.
    // (State-machine admissibility is the engine's, S2-04; this proves cross-version linking.)
    let v3 = runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let mut locked = lock(tx, 0x64).await?;
                let before = locked.audit_facts()?;
                let mut row = locked.row().clone();
                row.draft_revision += 1;
                locked.replace(&scope(0x64), row).await?;
                let after = locked.audit_facts()?;
                let pending = evidence(40).committed_transition(
                    Uuid::new_v4(),
                    AuditTrigger::Public(Trigger::DraftMutate),
                    &before,
                    &after,
                    None,
                )?;
                anyhow::Ok(writer::append_committed(&mut locked, pending).await?)
            })
        })
        .await?;
    assert_eq!((v3.row().hash_version, v3.row().sequence), (3, Some(3)));
    assert_eq!(
        v3.row().prev_hash.as_ref().map(Vec::len),
        Some(crate::domain::audit::DIGEST_LEN)
    );
    let v2_digest = vector("v2-after-v1-caller-reason")["sha256"]
        .as_str()
        .map(|hex| {
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect::<Vec<_>>()
        });
    assert_eq!(v3.row().prev_hash, v2_digest);
    assert_eq!(
        verify_stored(&pg, &verifier, 0x64).await??,
        3,
        "mixed v1/v2/v3 chain verifies"
    );
    anyhow::Ok(())
}

#[tokio::test]
async fn namespace_is_frozen_at_create_while_resource_tenant_is_snapshotted_per_entry()
-> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (runtime, _) = pg.role("runtime").await?;
    let (verifier, _) = pg.role("verifier").await?;
    create_audited(&runtime, 1).await?;
    // A draft edit moves the resource tenant; the chain namespace and genesis do not move.
    let moved = runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let mut locked = lock(tx, 1).await?;
                let before = locked.audit_facts()?;
                let mut row = locked.row().clone();
                row.resource_tenant_id = u(11);
                row.draft_revision += 1;
                locked.replace(&scope(1), row).await?;
                let after = locked.audit_facts()?;
                let pending = evidence(40).committed_transition(
                    Uuid::new_v4(),
                    AuditTrigger::Public(Trigger::DraftMutate),
                    &before,
                    &after,
                    None,
                )?;
                anyhow::Ok(writer::append_committed(&mut locked, pending).await?)
            })
        })
        .await?;
    assert_eq!(
        (moved.row().audit_tenant_id, moved.row().resource_tenant_id),
        (Some(u(10)), Some(u(11)))
    );
    assert_eq!(
        moved.row().subject_tenant_id,
        u(10),
        "actor home tenant kept separately"
    );
    assert_eq!(
        verify_stored(&pg, &verifier, 1).await??,
        2,
        "verified against the frozen namespace"
    );
    // An entry claiming another namespace for this aggregate is refused before insert.
    let foreign = runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let mut locked = lock(tx, 1).await?;
                let facts = locked.audit_facts()?;
                let claimed = crate::domain::audit::OrderFacts {
                    audit_tenant_id: u(11),
                    ..facts
                };
                let pending = evidence(40).committed_transition(
                    Uuid::new_v4(),
                    AuditTrigger::Public(Trigger::DraftMutate),
                    &claimed,
                    &claimed,
                    None,
                )?;
                anyhow::Ok(writer::append_committed(&mut locked, pending).await?)
            })
        })
        .await;
    assert!(matches!(
        foreign.unwrap_err().downcast::<AuditStoreError>(),
        Ok(AuditStoreError::Foreign)
    ));
    // An entry carrying a stale resource-tenant snapshot is refused as well.
    let stale = runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let mut locked = lock(tx, 1).await?;
                let facts = locked.audit_facts()?;
                let old = crate::domain::audit::OrderFacts {
                    resource_tenant_id: u(10),
                    ..facts
                };
                let pending = evidence(40).committed_transition(
                    Uuid::new_v4(),
                    AuditTrigger::Public(Trigger::DraftMutate),
                    &old,
                    &old,
                    None,
                )?;
                anyhow::Ok(writer::append_committed(&mut locked, pending).await?)
            })
        })
        .await;
    assert!(matches!(
        stale.unwrap_err().downcast::<AuditStoreError>(),
        Ok(AuditStoreError::Foreign)
    ));
    // The namespace column itself is immutable to the private writer role.
    let (_, private_raw) = pg.role("private").await?;
    assert!(private_raw
        .execute_unprepared("UPDATE bss_orders__order SET audit_tenant_id='00000000-0000-0000-0000-000000000011'")
        .await
        .is_err());
    anyhow::Ok(())
}

/// Audit and S2-05 settlement meet in one transaction: the registry records the sealed entry's
/// identity, and a failure after both rolls back both.
#[tokio::test]
async fn sealed_audit_and_registry_settlement_commit_or_roll_back_together() -> anyhow::Result<()> {
    use crate::domain::idempotency::{
        DraftRevisionInput, FingerprintAxes, FingerprintInput, FingerprintTarget, LeaseDuration,
        PrincipalScope, RegistryKey, RegistryOperation, Settlement, StoredResponse,
    };
    use crate::infra::storage::repo::idempotency::{
        self as reg, Durability, Gate, GateRequest, GateTarget,
    };
    let pg = Pg::new().await?;
    let (runtime, _) = pg.role("runtime").await?;
    let principal = PrincipalScope::from_context(&ctx(40))?;
    let request = |op: Trigger, key: &str, target: Option<u128>| {
        let k = RegistryKey::new(
            RegistryOperation::Trigger(op),
            principal.clone(),
            IdempotencyKey::try_from(key.to_owned()).unwrap(),
        );
        let doc = serde_json::json!({});
        let f = FingerprintInput {
            operation: if op == Trigger::Create {
                "create"
            } else {
                "submit"
            },
            trigger: op,
            target: target.map_or(FingerprintTarget::Create, |id| FingerprintTarget::Order {
                order_id: u(id),
                expected_version: 1,
            }),
            axes: FingerprintAxes {
                seller_tenant_id: u(20),
                resource_tenant_id: u(10),
                payer_tenant_id: u(30),
            },
            draft_revision: DraftRevisionInput::NotApplicable,
            contribution: &doc,
        }
        .fingerprint()
        .unwrap();
        (k, f)
    };
    let response = |status| {
        StoredResponse::new(
            status,
            serde_json::json!({}),
            std::collections::BTreeMap::new(),
            None,
        )
        .unwrap()
    };
    // Create: claim, aggregate + version + committed create audit, settle with that audit.
    let (k, f) = request(Trigger::Create, "create-1", None);
    let created = runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let scope_p = PrivateScope::for_principal(k.principal().as_str());
                let req = GateRequest {
                    key: &k,
                    fingerprint: &f,
                    lease: LeaseDuration::from_seconds(30)?,
                    durability: Durability::Ordinary,
                    presented: None,
                };
                let Gate::Owned(owned) =
                    reg::resolve(GateTarget::Create(tx), &scope_p, &req).await?
                else {
                    anyhow::bail!("not owned");
                };
                let row = repo::insert_order(tx, &scope(1), order(1)).await?;
                let mut locked = repo::LockedOrder::acquire(tx, &scope(1), &row).await?;
                locked.insert_order_version(version(1, 1, None)).await?;
                let pending =
                    evidence(40).committed_create(Uuid::new_v4(), &locked.audit_facts()?)?;
                let sealed = writer::append_committed(&mut locked, pending).await?;
                let settled = owned
                    .settle(Settlement::Success {
                        order_id: Some(u(1)),
                        audit_id: sealed.row().audit_id,
                        response: response(201),
                    })
                    .await?;
                anyhow::Ok((sealed, settled))
            })
        })
        .await?;
    assert_eq!(created.1.audit_id, Some(created.0.row().audit_id));
    // Resolved refusal on the locked order settles the refused key with its refusal audit.
    let (k, f) = request(Trigger::Submit, "submit-1", Some(1));
    let refused = runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let locked = lock(tx, 1).await?;
                let scope_p = PrivateScope::for_principal(k.principal().as_str());
                let req = GateRequest {
                    key: &k,
                    fingerprint: &f,
                    lease: LeaseDuration::from_seconds(30)?,
                    durability: Durability::Ordinary,
                    presented: None,
                };
                let Gate::Owned(owned) =
                    reg::resolve(GateTarget::Order(&locked), &scope_p, &req).await?
                else {
                    anyhow::bail!("not owned");
                };
                let sealed = evidence(40).resolved_refusal(
                    Uuid::new_v4(),
                    AuditTrigger::Public(Trigger::Submit),
                    &locked.audit_facts()?,
                    Reason::NotAdmissible,
                )?;
                let sealed = writer::append_resolved_refusal(&locked, sealed).await?;
                let settled = owned
                    .settle(Settlement::Refused {
                        reason: Reason::NotAdmissible,
                        audit_id: sealed.row().audit_id,
                        response: response(409),
                    })
                    .await?;
                anyhow::Ok((sealed, settled))
            })
        })
        .await?;
    assert_eq!(refused.1.audit_id, Some(refused.0.row().audit_id));
    // Claim + committed audit + settlement, then an injected failure: nothing survives.
    let (k, f) = request(Trigger::Submit, "submit-2", Some(1));
    let failed: anyhow::Result<()> = runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let mut locked = lock(tx, 1).await?;
                let scope_p = PrivateScope::for_principal(k.principal().as_str());
                let req = GateRequest {
                    key: &k,
                    fingerprint: &f,
                    lease: LeaseDuration::from_seconds(30)?,
                    durability: Durability::Ordinary,
                    presented: None,
                };
                let Gate::Owned(owned) =
                    reg::resolve(GateTarget::Order(&locked), &scope_p, &req).await?
                else {
                    anyhow::bail!("not owned");
                };
                let before = locked.audit_facts()?;
                let mut row = locked.row().clone();
                row.state = "submitted".into();
                locked.replace(&scope(1), row).await?;
                let pending = evidence(40).committed_transition(
                    Uuid::new_v4(),
                    AuditTrigger::Public(Trigger::Submit),
                    &before,
                    &locked.audit_facts()?,
                    None,
                )?;
                let sealed = writer::append_committed(&mut locked, pending).await?;
                owned
                    .settle(Settlement::Success {
                        order_id: Some(u(1)),
                        audit_id: sealed.row().audit_id,
                        response: response(200),
                    })
                    .await?;
                anyhow::bail!("injected failure after audit and settlement")
            })
        })
        .await;
    assert!(failed.is_err());
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__idempotency")
            .await?,
        2
    );
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__transition_audit")
            .await?,
        2
    );
    assert_eq!(
        pg.scalar("SELECT audit_sequence AS n FROM bss_orders__order")
            .await?,
        1
    );
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__order WHERE state='draft'")
            .await?,
        1
    );
    anyhow::Ok(())
}

/// Gap review: the counter has one writer. A business `replace` that moves `audit_sequence`
/// (forward, opening a chain gap, or back, reusing a position) is refused before SQL, and the
/// refused create with no target identifier stores its exact unresolved shape.
#[tokio::test]
async fn business_writes_cannot_move_the_audit_counter_and_refused_create_is_unresolved()
-> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (runtime, _) = pg.role("runtime").await?;
    let (verifier, _) = pg.role("verifier").await?;
    create_audited(&runtime, 1).await?;
    mutate(&runtime, 1).await?;
    for moved in [3_i64, 1, 0] {
        let refused = runtime
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let mut locked = lock(tx, 1).await?;
                    let mut row = locked.row().clone();
                    row.draft_revision += 1;
                    row.audit_sequence = moved;
                    anyhow::Ok(locked.replace(&scope(1), row).await?)
                })
            })
            .await;
        assert!(
            matches!(
                refused
                    .unwrap_err()
                    .downcast::<toolkit_db::secure::ScopeError>(),
                Ok(toolkit_db::secure::ScopeError::Invalid(_))
            ),
            "counter moved to {moved} without a sealed append"
        );
    }
    assert_eq!(
        pg.scalar("SELECT audit_sequence AS n FROM bss_orders__order")
            .await?,
        2
    );
    assert_eq!(
        pg.scalar("SELECT draft_revision AS n FROM bss_orders__order")
            .await?,
        1,
        "the refused business write changed nothing"
    );
    assert_eq!(verify_stored(&pg, &verifier, 1).await??, 2);
    // A refused create before any identifier exists: both references NULL, subject tenant kept.
    let sealed = evidence(40).unresolved_refusal(
        Uuid::new_v4(),
        AuditTrigger::Public(Trigger::Create),
        None,
        Reason::OperationNotPermittedForActor,
    )?;
    let appended = runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                anyhow::Ok(
                    writer::append_unresolved_refusal(
                        tx,
                        &PrivateScope::for_refusal(&caller(40)),
                        sealed,
                    )
                    .await?,
                )
            })
        })
        .await?;
    let (row, hash) = stored(&pg, appended.row().audit_id).await?;
    assert_eq!(&row, appended.row());
    verify_entry(&row, &hash)?;
    assert_eq!(
        (row.order_id, row.requested_order_ref, row.audit_tenant_id),
        (None, None, None)
    );
    assert_eq!(
        (row.trigger.as_str(), row.subject_tenant_id),
        ("create", u(10))
    );
    // An order-targeted refusal can never drop its requested identifier.
    assert!(
        evidence(40)
            .unresolved_refusal(
                Uuid::new_v4(),
                AuditTrigger::Public(Trigger::Submit),
                None,
                Reason::OrderNotFound,
            )
            .is_err()
    );
    anyhow::Ok(())
}

/// Gap review: the frozen refusal vectors, the S2-02 row CHECKs and the runtime verifier agree.
/// A resolved refusal keeps its observed state (`to_state = from_state`, §3.7/D-98); the former
/// NULL `to_state` vector shape is exactly what the DDL refuses.
#[tokio::test]
async fn refusal_vectors_store_as_rows_and_verify_against_the_ddl() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    create(&pg.db, 0x64).await?;
    let fixture = conformance_vectors()?;
    // Vectors are authored for subject tenant 0x14; read back under that refusal scope.
    let reader = PrivateScope::for_refusal(&crate::authz::Caller::new(
        SecurityContext::builder()
            .subject_id(u(0x1e))
            .subject_tenant_id(u(0x14))
            .build()?,
        None,
    ));
    for id in [
        "v1-resolved-refusal",
        "v2-resolved-refusal",
        "v3-resolved-refusal",
        "v1-unresolved-refusal",
        "v2-unresolved-refusal",
        "v3-unresolved-refusal",
        "v3-force-request",
    ] {
        let v = conformance_vector(&fixture, id);
        pg.sql(&vector_row_sql(&v)).await?;
        let audit_id = Uuid::parse_str(v["input"]["audit_id"].as_str().unwrap())?;
        let model =
            repo::private::find_transition_audit(&pg.db.conn()?, reader.access_scope(), audit_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("{id} not stored"))?;
        let (row, hash) = from_model(&model)?;
        let digest = verify_entry(&row, &hash).map_err(|e| anyhow::anyhow!("{id}: {e}"))?;
        let authored = v["sha256"].as_str().unwrap();
        let authored: Vec<u8> = (0..authored.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&authored[at..at + 2], 16).unwrap())
            .collect();
        assert_eq!(
            digest.to_vec(),
            authored,
            "{id}: runtime digest equals the Python digest"
        );
        if row.order_id.is_some() {
            assert_eq!(row.to_state, row.from_state, "{id}");
        }
        // Several vectors share one audit identity; remove this one (owner-only fixture cleanup).
        for statement in [
            "ALTER TABLE bss_orders__transition_audit DISABLE TRIGGER bss_orders__transition_audit_delete".to_owned(),
            format!("DELETE FROM bss_orders__transition_audit WHERE audit_id='{audit_id}'"),
            "ALTER TABLE bss_orders__transition_audit ENABLE TRIGGER bss_orders__transition_audit_delete".to_owned(),
        ] {
            pg.sql(&statement).await?;
        }
    }
    // The pre-correction shape (resolved refusal with NULL to_state) is refused by the DDL.
    let mut stale = conformance_vector(&fixture, "v3-resolved-refusal");
    stale["input"]["to_state"] = serde_json::Value::Null;
    assert!(pg.sql(&vector_row_sql(&stale)).await.is_err());
    anyhow::Ok(())
}
