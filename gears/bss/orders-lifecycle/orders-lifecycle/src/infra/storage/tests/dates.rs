//! S2-10 on real PostgreSQL: the date-policy channel (promotion through the policy role, the
//! database revision guards and the startup default check), the frozen date basis read with the
//! database clock, and admission through the S2-04 engine's submit row with the
//! `capture.date-basis` guard checked against its single transition timestamp under the lock.
//!
//! Draft lines are authored through the real S2-09 capture service. Submit itself is S3-12: the
//! test supplies the version contribution and binds every other row-4 guard as passing, exactly
//! as the engine suite does; the date guard, the composite gate's date failures and the
//! admitted line rows are the S2-10 implementation.
use super::engine::{T, buyer, create, fixed, key, prepared, request, reserve_with_dates};
use super::*;
use crate::config::{DatePolicyConfig, DatePolicyOverrideConfig, DatePolicySwitches};
use crate::domain::dates::{
    DateBasis, DateField, DateSource, PLATFORM_DEFAULT_POLICY_ID, PolicyFault, PolicyScope,
    SuppliedDates, utc_date,
};
use crate::domain::guards::GATE_COMPOSITE;
use crate::domain::transition::{GuardSubject, GuardVerdict};
use crate::infra::capture::{CaptureService, CaptureSettings, WriteMeta};
use crate::infra::dates::{
    AdmittedLineError, DatePolicyPlan, DatePreparer, PolicyStoreError, PreparationClock,
    PrepareDatesError, admitted_line, install, promote, supplied_from_admitted,
    supplied_from_draft, verify_platform_default,
};
use crate::infra::engine::{ChildDocuments, DocumentWriter, OutcomeKind, Prepared};
use arc_swap::ArcSwapOption;
use async_trait::async_trait;
use bss_orders_lifecycle_sdk::authoring::{
    AddLine, AuthoredTerm, BillingCycle, CalendarDate, Currency, SelectedItem,
};
use bss_orders_lifecycle_sdk::catalog::{OrderState as S, Reason, Trigger};
use bss_orders_lifecycle_sdk::models::{CallMeta, DraftRevision, OrderVersion};
use serde_json::{Value, json};
use std::sync::Arc;
use time::macros::date;
use toolkit_db::secure::ScopeError;
use toolkit_security::AccessScope;

const TENANT: u128 = 10;

fn switches(sa: bool, ad: bool) -> DatePolicySwitches {
    DatePolicySwitches {
        service_activation_required: sa,
        acceptance_due_required: ad,
    }
}
fn plan(
    default: (bool, bool),
    overrides: &[(u128, bool, bool)],
    retired: &[u128],
) -> DatePolicyPlan {
    DatePolicyPlan::from_config(&DatePolicyConfig {
        platform_default: switches(default.0, default.1),
        overrides: overrides
            .iter()
            .map(|(t, sa, ad)| DatePolicyOverrideConfig {
                resource_tenant_id: u(*t),
                service_activation_required: *sa,
                acceptance_due_required: *ad,
            })
            .collect(),
        retired_overrides: retired.iter().map(|t| u(*t)).collect(),
    })
    .unwrap()
}
/// `(tenant or 0 for the default, sa, ad, revision, policy_id)` rows, default first.
async fn rows(pg: &Pg) -> Vec<(u128, bool, bool, i64, Uuid)> {
    let rows = pg
        .raw
        .query_all_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT resource_tenant_id, service_activation_required AS sa, acceptance_due_required AS ad, revision, policy_id FROM bss_orders__date_policy ORDER BY resource_tenant_id NULLS FIRST",
        ))
        .await
        .unwrap();
    rows.iter()
        .map(|r| {
            let tenant: Option<Uuid> = r.try_get("", "resource_tenant_id").unwrap();
            (
                tenant.map_or(0, |t| t.as_u128()),
                r.try_get("", "sa").unwrap(),
                r.try_get("", "ad").unwrap(),
                r.try_get("", "revision").unwrap(),
                r.try_get("", "policy_id").unwrap(),
            )
        })
        .collect()
}
fn revisions(rows: &[(u128, bool, bool, i64, Uuid)]) -> Vec<(u128, i64)> {
    rows.iter().map(|r| (r.0, r.3)).collect()
}

#[test]
fn promotion_config_is_explicit_and_validated() {
    let yaml = |s: &str| serde_saphyr::from_str::<DatePolicyConfig>(s);
    // Switches never default.
    assert!(yaml("platform_default: {service_activation_required: true}").is_err());
    assert!(
        yaml("platform_default: {service_activation_required: true, acceptance_due_required: false, extra: 1}")
            .is_err()
    );
    let ok = yaml(
        "platform_default: {service_activation_required: false, acceptance_due_required: true}\noverrides:\n  - {resource_tenant_id: 00000000-0000-0000-0000-00000000000a, service_activation_required: true, acceptance_due_required: false}",
    )
    .unwrap();
    assert!(DatePolicyPlan::from_config(&ok).is_ok());
    let twice = DatePolicyConfig {
        overrides: vec![ok.overrides[0], ok.overrides[0]],
        ..ok.clone()
    };
    assert!(DatePolicyPlan::from_config(&twice).is_err());
    let both = DatePolicyConfig {
        retired_overrides: vec![u(10)],
        ..ok.clone()
    };
    assert!(DatePolicyPlan::from_config(&both).is_err());
    let nil = DatePolicyConfig {
        retired_overrides: vec![Uuid::nil()],
        ..ok
    };
    assert!(DatePolicyPlan::from_config(&nil).is_err());
}

/// The policy channel: promote a default change, two overrides, a change and a retirement through
/// the policy role. Every write advances the namespace high-water mark; an unchanged promotion is
/// read-only (so a runtime-role boot with the same config succeeds), and a refused promotion
/// leaves every row unchanged.
#[tokio::test]
async fn the_policy_channel_promotes_atomically_with_monotonic_revisions() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (policy, _) = pg.role("policy").await?;
    let (runtime, _) = pg.role("runtime").await?;
    let snap = verify_platform_default(&runtime).await?;
    assert_eq!(
        (
            snap.scope,
            snap.revision,
            snap.service_activation_required,
            snap.acceptance_due_required
        ),
        (PolicyScope::PlatformDefault, 1, false, false)
    );

    let first = plan((true, false), &[(11, false, true), (12, true, true)], &[]);
    // The runtime role cannot write policy: the whole promotion is refused with nothing written.
    let before = rows(&pg).await;
    assert!(matches!(
        promote(&runtime, &first).await,
        Err(PolicyStoreError::Scope(_) | PolicyStoreError::Db(_))
    ));
    assert_eq!(rows(&pg).await, before);

    let report = promote(&policy, &first).await?;
    assert!(report.platform_default_changed);
    assert_eq!(
        (report.overrides_inserted, report.overrides_updated),
        (2, 0)
    );
    let after = rows(&pg).await;
    // Default 1 -> 2 (switch change); tenant 11 inserted at 3 (default -> 4); tenant 12 at 5
    // (default -> 6).
    assert_eq!(revisions(&after), vec![(0, 6), (11, 3), (12, 5)]);
    assert_eq!((after[0].1, after[0].2), (true, false));
    assert_eq!((after[1].1, after[1].2), (false, true));
    assert_eq!(after[0].4, PLATFORM_DEFAULT_POLICY_ID);

    // Same promotion again: no write, so the runtime role succeeds and nothing changes.
    assert!(!promote(&runtime, &first).await?.changed());
    assert!(!promote(&policy, &first).await?.changed());
    install(&runtime, Some(&first)).await?;
    assert_eq!(rows(&pg).await, after);

    // Change 11, retire 12. 11 keeps its identity with a revision above the high-water mark.
    let second = plan((true, false), &[(11, true, true)], &[12]);
    let report = promote(&policy, &second).await?;
    assert_eq!(
        (
            report.platform_default_changed,
            report.overrides_updated,
            report.overrides_retired
        ),
        (false, 1, 1)
    );
    let later = rows(&pg).await;
    assert_eq!(revisions(&later), vec![(0, 9), (11, 7)]);
    assert_eq!(later[1].4, after[1].4);
    // Re-creating 12 takes a fresh identity and a revision above any the namespace carried.
    promote(
        &policy,
        &plan((true, false), &[(11, true, true), (12, false, false)], &[]),
    )
    .await?;
    let recreated = rows(&pg).await;
    assert_eq!(revisions(&recreated), vec![(0, 11), (11, 7), (12, 10)]);
    assert_ne!(recreated[2].4, after[2].4);
    // Retiring an absent override is a no-op; no Orders endpoint exists for any of this.
    assert!(
        !promote(
            &policy,
            &plan(
                (true, false),
                &[(11, true, true), (12, false, false)],
                &[13]
            )
        )
        .await?
        .changed()
    );
    Ok(())
}

/// Two replicas booting with the same configuration both start: a promotion that waits behind a
/// concurrent one re-reads the rows that one committed, under the namespace lock, instead of
/// inserting a duplicate override from the snapshot taken before it waited (READ COMMITTED
/// `FOR UPDATE` never returns rows inserted after its statement began).
#[tokio::test]
async fn a_promotion_waiting_behind_a_concurrent_one_sees_its_committed_rows() -> anyhow::Result<()>
{
    use sea_orm::TransactionTrait;
    let pg = Pg::new().await?;
    let (policy, policy_raw) = pg.role("policy").await?;
    // Replica A: inserts the override and holds the namespace (default row) lock, uncommitted.
    let a = policy_raw.begin().await?;
    a.execute_unprepared(&format!(
        "INSERT INTO bss_orders__date_policy VALUES('{}','{}',true,false,2,clock_timestamp())",
        u(700),
        u(TENANT)
    ))
    .await?;
    // Replica B: the same plan. Its unlocked read cannot see A's row, so it must write, and
    // its transaction waits on the namespace lock.
    let same = plan((false, false), &[(TENANT, true, false)], &[]);
    let b = tokio::spawn({
        let policy = policy.clone();
        async move { promote(&policy, &same).await }
    });
    let mut waited = false;
    for _ in 0..200 {
        if pg
            .scalar("SELECT count(*) AS n FROM pg_stat_activity WHERE wait_event_type='Lock' AND usename='test_policy'")
            .await?
            > 0
        {
            waited = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(waited, "replica B never waited on the namespace lock");
    a.commit().await?;
    let report = b.await??;
    assert!(!report.changed(), "{report:?}");
    let stored = rows(&pg).await;
    assert_eq!(revisions(&stored), vec![(0, 3), (TENANT, 2)]);
    assert_eq!(stored[1].4, u(700));
    Ok(())
}

/// Startup refuses a missing platform default; preparation fails the date guard rather than
/// inventing switches. (Removing the default needs owner privilege with triggers disabled.)
#[tokio::test]
async fn a_missing_platform_default_fails_startup_and_preparation() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (runtime, _) = pg.role("runtime").await?;
    pg.sql("ALTER TABLE bss_orders__date_policy DISABLE TRIGGER USER; DELETE FROM bss_orders__date_policy; ALTER TABLE bss_orders__date_policy ENABLE TRIGGER USER").await?;
    assert!(matches!(
        verify_platform_default(&runtime).await,
        Err(PolicyStoreError::Fault(PolicyFault::Missing))
    ));
    assert!(install(&runtime, None).await.is_err());
    let preparer = DatePreparer::new(runtime.clone(), PreparationClock::Database);
    assert!(matches!(
        preparer.prepare(u(TENANT), []).await,
        Err(PrepareDatesError::Policy(PolicyFault::Missing))
    ));
    // A tenant override alone does not stand in for the default.
    pg.sql(&format!("ALTER TABLE bss_orders__date_policy DISABLE TRIGGER USER; INSERT INTO bss_orders__date_policy VALUES('{}','{}',false,false,3,now()); ALTER TABLE bss_orders__date_policy ENABLE TRIGGER USER", u(500), u(TENANT))).await?;
    assert!(matches!(
        preparer.prepare(u(TENANT), []).await,
        Err(PrepareDatesError::Policy(PolicyFault::Missing))
    ));
    Ok(())
}

/// The snapshot is the tenant's own row when present, else the default, read once with the
/// database clock that also stamps the engine's transition timestamp.
#[tokio::test]
async fn preparation_snapshots_the_effective_row_and_the_database_utc_date() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (policy, _) = pg.role("policy").await?;
    let (runtime, _) = pg.role("runtime").await?;
    promote(&policy, &plan((false, true), &[(TENANT, true, false)], &[])).await?;
    let preparer = DatePreparer::new(runtime, PreparationClock::Database);
    let line = u(1);
    let basis = preparer
        .prepare(u(TENANT), [(line, SuppliedDates::default())])
        .await?;
    assert_eq!(basis.policy.scope, PolicyScope::ResourceTenant);
    assert_eq!(basis.policy.resource_tenant_id, Some(u(TENANT)));
    assert!(basis.policy.service_activation_required && !basis.policy.acceptance_due_required);
    let today = pg
        .scalar("SELECT (to_char((clock_timestamp() AT TIME ZONE 'UTC')::date,'YYYYMMDD'))::bigint AS n")
        .await?;
    let proposed = basis.proposed_utc_date.0;
    let proposed_n = i64::from(proposed.year()) * 10_000
        + i64::from(u8::from(proposed.month())) * 100
        + i64::from(proposed.day());
    // The UTC day could roll between the two statements only in a sub-second window.
    assert!((today - proposed_n).abs() <= 1, "{today} vs {proposed}");
    assert_eq!(basis.failures(), vec![(line, DateField::ServiceActivation)]);
    let other = preparer
        .prepare(u(11), [(line, SuppliedDates::default())])
        .await?;
    assert_eq!(other.policy.scope, PolicyScope::PlatformDefault);
    assert_eq!(other.failures(), vec![(line, DateField::AcceptanceDue)]);
    assert!(matches!(
        preparer.prepare(Uuid::nil(), []).await,
        Err(PrepareDatesError::Policy(PolicyFault::Invalid))
    ));
    Ok(())
}

// ------------------------------------------------------------------------------------------
// Admission through the engine (row 4)

fn write(text: &str, revision: i64) -> WriteMeta {
    WriteMeta {
        call: CallMeta {
            expected_version: OrderVersion::try_from(1).unwrap(),
            idempotency_key: key(text),
            correlation_id: None,
            delegation_proof_ref: None,
        },
        expected_draft_revision: Some(DraftRevision::try_from(revision).unwrap()),
    }
}
fn add_line(dates: [Option<time::Date>; 3], term: AuthoredTerm) -> AddLine {
    AddLine {
        plan_id: u(200),
        plan_revision_id: u(201),
        selected_items: vec![SelectedItem {
            item_id: u(300),
            quantity: Some(serde_json::from_value(json!("3")).unwrap()),
            selected_dim_value: None,
        }],
        currency: Currency::try_from("EUR".to_owned()).unwrap(),
        contract_effective_date: dates[0].map(CalendarDate),
        service_activation_date: dates[1].map(CalendarDate),
        acceptance_due_date: dates[2].map(CalendarDate),
        term_duration: Some(term),
        billing_cycle: Some(BillingCycle::Month),
    }
}

/// Writes every admitted line from its draft line and the frozen basis.
struct AdmitLines {
    drafts: Vec<entity::draft_content::Model>,
    basis: Arc<DateBasis>,
    version: i32,
}
#[async_trait]
impl DocumentWriter for AdmitLines {
    async fn write(&self, docs: &ChildDocuments<'_, '_>) -> Result<(), ScopeError> {
        for draft in &self.drafts {
            let row = admitted_line(draft, u(TENANT), self.version, &self.basis)
                .map_err(|_| ScopeError::Invalid("unresolved line"))?;
            docs.insert_order_line(row).await?;
        }
        Ok(())
    }
}

struct Draft {
    order: Uuid,
    lines: Vec<entity::draft_content::Model>,
}

/// A draft with two authored lines through the real capture service: one fully dated with a
/// calendar term, one with no dates and a 12-period term.
async fn authored_draft(t: &T, text: &str) -> Draft {
    let engine = t.engine(&[]);
    let order = create(t, &engine, &format!("{text}-create")).await;
    let slot = Arc::new(ArcSwapOption::from(Some(Arc::new(engine))));
    let capture = CaptureService::new(slot, CaptureSettings { line_cap: 200 });
    capture
        .add_line(
            &buyer(),
            order,
            add_line(
                [
                    Some(date!(2026 - 12 - 01)),
                    Some(date!(2027 - 01 - 15)),
                    Some(date!(2026 - 11 - 20)),
                ],
                AuthoredTerm::Calendar {
                    years: 1,
                    months: 2,
                    days: 3,
                    microseconds: 4,
                },
            ),
            &write(&format!("{text}-l1"), 0),
        )
        .await
        .unwrap();
    capture
        .add_line(
            &buyer(),
            order,
            add_line([None, None, None], AuthoredTerm::Periods { count: 12 }),
            &write(&format!("{text}-l2"), 1),
        )
        .await
        .unwrap();
    let mut lines = repo::children::draft_content_for_order(
        &t.env.db.conn().unwrap(),
        &AccessScope::for_resources(vec![order]),
        order,
    )
    .await
    .unwrap();
    lines.sort_by_key(|l| l.contract_effective_date.is_none());
    // Draft authoring never cascades: unauthored dates stay NULL.
    assert_eq!(lines[1].contract_effective_date, None);
    assert_eq!(lines[1].service_activation_date, None);
    Draft { order, lines }
}

async fn basis(t: &T, draft: &Draft, clock: PreparationClock) -> DateBasis {
    DatePreparer::new(t.env.db.clone(), clock)
        .prepare(
            u(TENANT),
            draft
                .lines
                .iter()
                .map(|l| (l.line_id, supplied_from_draft(l))),
        )
        .await
        .unwrap()
}

/// Submit `draft` with `basis` bound to `capture.date-basis` and the composite gate refusing
/// exactly when the basis reports missing required dates (03 §4.2 predicate 8).
async fn submit(
    t: &T,
    draft: &Draft,
    basis: &DateBasis,
    text: &str,
) -> crate::infra::engine::EngineOutcome {
    let engine = t.engine(&[]);
    let mut req = request(draft.order, Trigger::Submit, text, 1);
    req.expected_draft_revision = Some(2);
    let (candidate, req) = reserve_with_dates(&engine, &buyer(), &req, basis.frozen()).await;
    let shared = Arc::new(basis.clone());
    let mut p: Prepared = prepared(4, draft.order, S::Draft, Some(candidate));
    let failures = basis.failures();
    p.guards = p
        .guards
        .bind("capture.date-basis", Arc::clone(&shared).guard())
        .bind(GATE_COMPOSITE, move |_: &GuardSubject<'_>| {
            if failures.is_empty() {
                GuardVerdict::Pass
            } else {
                GuardVerdict::Fail(Reason::DateCascadeInvalid)
            }
        });
    p.documents = Some(Arc::new(AdmitLines {
        drafts: draft.lines.clone(),
        basis: shared,
        version: candidate,
    }));
    engine.transition(&buyer(), req, &fixed(p)).await.unwrap()
}

async fn order_lines(t: &T, order: Uuid) -> Vec<entity::order_line::Model> {
    let mut lines = repo::children::order_line_for_order(
        &t.env.db.conn().unwrap(),
        &AccessScope::for_resources(vec![order]),
        order,
    )
    .await
    .unwrap();
    // The fully dated line first (its contract-effective date was authored).
    lines.sort_by_key(|l| l.contract_effective_date != date!(2026 - 12 - 01));
    lines
}

/// Admission stores three resolved dates and the identical snapshot, copies the authored term
/// verbatim, writes no acceptance from the due date, and survives a later policy promotion and
/// later reads unchanged. The attempt's frozen basis restores exactly.
#[tokio::test]
async fn admission_persists_the_resolved_triple_and_snapshot_immutably() {
    let t = T::new().await;
    let (policy, _) = t.env.pg.role("policy").await.unwrap();
    promote(
        &policy,
        &plan((false, false), &[(TENANT, false, false)], &[]),
    )
    .await
    .unwrap();
    let draft = authored_draft(&t, "ok").await;
    let basis = basis(&t, &draft, PreparationClock::Database).await;
    assert!(basis.failures().is_empty());
    let out = submit(&t, &draft, &basis, "ok-submit").await;
    let OutcomeKind::Committed { .. } = out.kind else {
        panic!("{out:?}")
    };
    let lines = order_lines(&t, draft.order).await;
    assert_eq!(lines.len(), 2);
    let today = basis.proposed_utc_date.0;
    let (dated, undated) = (&lines[0], &lines[1]);
    assert_eq!(
        (
            dated.contract_effective_date,
            dated.service_activation_date,
            dated.acceptance_due_date
        ),
        (
            date!(2026 - 12 - 01),
            Some(date!(2027 - 01 - 15)),
            Some(date!(2026 - 11 - 20))
        )
    );
    // Unauthored: contract-effective is the transition date; dependents default to it.
    assert_eq!(
        (
            undated.contract_effective_date,
            undated.service_activation_date,
            undated.acceptance_due_date
        ),
        (today, Some(today), Some(today))
    );
    assert_eq!(
        basis
            .line(undated.line_id)
            .unwrap()
            .contract_effective
            .source,
        DateSource::TransitionDate
    );
    for line in &lines {
        assert_eq!(line.date_policy_switch_state, basis.policy.switch_state());
        let draft_line = draft
            .lines
            .iter()
            .find(|d| d.line_id == line.line_id)
            .unwrap();
        // Exact term preservation (D-193): intent, kind, duration and cycle.
        assert_eq!(line.authored_term, draft_line.authored_term);
        assert_eq!(line.term_kind, draft_line.term_kind);
        assert_eq!(line.term_duration, draft_line.term_duration);
        assert_eq!(Some(line.billing_cycle.clone()), draft_line.billing_cycle);
    }
    assert_eq!(
        t.json(&format!("SELECT authored_term AS j FROM bss_orders__order_line WHERE order_id='{}' AND term_kind='finite' AND authored_term->>'kind'='calendar'", draft.order)).await,
        json!({"kind": "calendar", "years": 1, "months": 2, "days": 3, "microseconds": 4})
    );
    assert_eq!(
        t.n(&format!("SELECT count(*) AS n FROM bss_orders__order_line WHERE order_id='{}' AND term_duration = interval '1 year 2 months 3 days 0.000004 seconds'", draft.order)).await,
        1
    );
    // The due date is not assent: no acceptance was recorded.
    assert_eq!(
        t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__acceptance WHERE order_id='{}'",
            draft.order
        ))
        .await,
        0
    );
    // The frozen D-188 basis restores exactly; recovery never re-reads current policy.
    let frozen = t
        .json(&format!(
            "SELECT date_policy_basis AS j FROM bss_orders__commercial_attempt WHERE order_id='{}'",
            draft.order
        ))
        .await;
    assert_eq!(DateBasis::from_frozen(&frozen).unwrap(), basis);
    // The builder refuses a basis for another resource tenant, and a basis whose supplied
    // values are not the line's current authored dates (a draft edited after preparation).
    assert_eq!(
        admitted_line(&draft.lines[0], u(11), 2, &basis),
        Err(AdmittedLineError::ForeignBasis)
    );
    let mut edited = draft.lines[0].clone();
    edited.service_activation_date = Some(date!(2027 - 02 - 01));
    assert_eq!(
        admitted_line(&edited, u(TENANT), 2, &basis),
        Err(AdmittedLineError::AuthoredMismatch)
    );
    edited.service_activation_date = None;
    assert_eq!(
        admitted_line(&edited, u(TENANT), 2, &basis),
        Err(AdmittedLineError::AuthoredMismatch)
    );
    let mut authored_later = draft.lines[1].clone();
    authored_later.contract_effective_date = Some(basis.proposed_utc_date.0);
    assert_eq!(
        admitted_line(&authored_later, u(TENANT), 2, &basis),
        Err(AdmittedLineError::AuthoredMismatch)
    );

    // A later promotion changes the tenant row; admitted lines keep the original snapshot and
    // values, reads do not recompute them, and an amendment's carried values stay values.
    promote(&policy, &plan((false, false), &[(TENANT, true, true)], &[]))
        .await
        .unwrap();
    let reread = order_lines(&t, draft.order).await;
    assert_eq!(reread, lines);
    let snapshot: crate::domain::dates::DatePolicySnapshot =
        serde_json::from_value(reread[0].date_policy_switch_state.clone()).unwrap();
    assert!(!snapshot.service_activation_required);
    assert_eq!(snapshot.revision, basis.policy.revision);
    let amendment = DatePreparer::new(t.env.db.clone(), PreparationClock::Database)
        .prepare(
            u(TENANT),
            reread
                .iter()
                .map(|l| (l.line_id, supplied_from_admitted(l))),
        )
        .await
        .unwrap();
    assert!(amendment.policy.service_activation_required);
    assert!(amendment.policy.revision > basis.policy.revision);
    assert!(amendment.failures().is_empty());
    for line in &reread {
        let carried = amendment.line(line.line_id).unwrap();
        assert_eq!(carried.contract_effective.source, DateSource::Supplied);
        assert_eq!(
            carried.contract_effective.date(),
            line.contract_effective_date
        );
    }
    check_shapes(&t, draft.order, &basis.policy.switch_state()).await;
    // Admitted rows are append-only evidence for the runtime role.
    let runtime = Database::connect(format!(
        "postgres://test_runtime:fixture@127.0.0.1:{}/postgres",
        t.env.pg.port
    ))
    .await
    .unwrap();
    assert!(
        runtime
            .execute_unprepared("UPDATE bss_orders__order_line SET service_activation_date = service_activation_date + 1")
            .await
            .is_err()
    );
}

/// The admitted-line CHECK (D-207 item 3, D-208): the exact refused and allowed shapes.
async fn check_shapes(t: &T, order: Uuid, state: &Value) {
    // The schema refuses an admitted line without all three dates or with a malformed snapshot.
    let extra = u(9_001);
    t.env
        .pg
        .sql(&format!(
            "INSERT INTO bss_orders__order_line_identity VALUES('{order}','{extra}',now())"
        ))
        .await
        .unwrap();
    let insert = |sa: &str, ad: &str, snapshot: &Value| {
        format!(
            "INSERT INTO bss_orders__order_line(order_id,version,line_id,plan_id,plan_revision_id,selected_items,currency,contract_effective_date,service_activation_date,acceptance_due_date,term_duration,term_kind,authored_term,billing_cycle,date_policy_switch_state) \
             SELECT order_id,version,'{extra}',plan_id,plan_revision_id,selected_items,currency,contract_effective_date,{sa},{ad},term_duration,term_kind,authored_term,billing_cycle,'{snapshot}'::jsonb FROM bss_orders__order_line WHERE order_id='{order}' LIMIT 1"
        )
    };
    let mut tampered = Vec::new();
    for (key, value) in [
        ("revision", json!(0)),
        ("revision", json!("1")),
        ("scope", json!("platform_default")),
        ("scope", json!("tenant")),
        ("resource_tenant_id", json!(null)),
        ("policy_id", json!(PLATFORM_DEFAULT_POLICY_ID)),
        ("service_activation_required", json!("false")),
        ("extra", json!(1)),
    ] {
        let mut v = state.clone();
        v[key] = value;
        tampered.push(v);
    }
    let mut missing = state.clone();
    missing.as_object_mut().unwrap().remove("revision");
    tampered.push(missing);
    tampered.push(json!({}));
    for snapshot in &tampered {
        assert!(
            t.env
                .pg
                .sql(&insert("CURRENT_DATE", "CURRENT_DATE", snapshot))
                .await
                .is_err(),
            "{snapshot}"
        );
    }
    // D-208: no admitted shape stores a NULL dependent date, whatever the snapshot scope.
    let default_state = json!({
        "service_activation_required": false, "acceptance_due_required": false,
        "scope": "platform_default", "resource_tenant_id": null,
        "policy_id": PLATFORM_DEFAULT_POLICY_ID, "revision": 1
    });
    for snapshot in [state, &default_state] {
        for (sa, ad) in [
            ("NULL", "CURRENT_DATE"),
            ("CURRENT_DATE", "NULL"),
            ("NULL", "NULL"),
        ] {
            assert!(
                t.env.pg.sql(&insert(sa, ad, snapshot)).await.is_err(),
                "{sa} {ad} {snapshot}"
            );
        }
    }
    // A platform-default snapshot under a tenant scope's identity, and vice versa, is refused.
    let mut crossed = default_state.clone();
    crossed["policy_id"] = json!(u(500));
    assert!(
        t.env
            .pg
            .sql(&insert("CURRENT_DATE", "CURRENT_DATE", &crossed))
            .await
            .is_err()
    );
    // Allowed: both dates present with a well-formed tenant or platform-default snapshot.
    t.env
        .pg
        .sql(&insert("CURRENT_DATE", "CURRENT_DATE", state))
        .await
        .unwrap();
    let extra2 = u(9_002);
    t.env
        .pg
        .sql(&format!(
            "INSERT INTO bss_orders__order_line_identity VALUES('{order}','{extra2}',now())"
        ))
        .await
        .unwrap();
    t.env
        .pg
        .sql(
            &insert("CURRENT_DATE", "CURRENT_DATE", &default_state)
                .replace(&extra.to_string(), &extra2.to_string()),
        )
        .await
        .unwrap();
}

/// Admitted lines, versions past 1 and resolved totals of `order`.
async fn no_admission(t: &T, order: Uuid) -> i64 {
    t.n(&format!(
        "SELECT (SELECT count(*) FROM bss_orders__order_line WHERE order_id='{order}') + (SELECT count(*) FROM bss_orders__order_version WHERE order_id='{order}' AND version>1) + (SELECT count(*) FROM bss_orders__resolved_total WHERE order_id='{order}') AS n"
    ))
    .await
}

/// Injected clock across UTC midnight: the basis was prepared on the previous UTC day, so the
/// engine's transition timestamp refuses `date-cascade-invalid` with no admitted version, line
/// or snapshot; the same key replays the refusal; a fresh attempt re-resolves and commits.
#[tokio::test]
async fn a_utc_day_rollover_refuses_without_admission_and_a_fresh_attempt_commits() {
    let t = T::new().await;
    let draft = authored_draft(&t, "roll").await;
    let db_now = time::OffsetDateTime::now_utc();
    let stale = basis(
        &t,
        &draft,
        PreparationClock::Fixed(db_now - time::Duration::days(1)),
    )
    .await;
    assert_ne!(stale.proposed_utc_date.0, utc_date(db_now));
    let out = submit(&t, &draft, &stale, "roll-1").await;
    let OutcomeKind::Refused { reason, .. } = out.kind else {
        panic!("{out:?}")
    };
    assert_eq!(reason, Reason::DateCascadeInvalid);
    assert_eq!(out.response.status, 400);
    assert_eq!(
        out.response.body["error_code"],
        json!("DATE_CASCADE_INVALID")
    );
    assert_eq!(no_admission(&t, draft.order).await, 0);
    // D-208: the Problem carries no variant data; the stale basis is identified by the settled
    // evidence instead: the attempt's frozen basis and the refusal audit's transition timestamp.
    assert!(
        out.response.body["context"]["data"]
            .as_object()
            .is_none_or(serde_json::Map::is_empty),
        "{}",
        out.response.body
    );
    let frozen = t
        .json(&format!(
            "SELECT date_policy_basis AS j FROM bss_orders__commercial_attempt WHERE order_id='{}'",
            draft.order
        ))
        .await;
    assert_eq!(DateBasis::from_frozen(&frozen).unwrap(), stale);
    let refused_on = t
        .json(&format!(
            "SELECT to_jsonb((created_at AT TIME ZONE 'UTC')::date::text) AS j FROM bss_orders__transition_audit WHERE order_id='{}' AND outcome='refused' AND reason='date-cascade-invalid'",
            draft.order
        ))
        .await;
    assert_ne!(
        refused_on,
        json!(stale.proposed_utc_date.0.to_string()),
        "the refusal instant's UTC day differs from the frozen proposed date"
    );
    let row = t
        .json(&format!(
            "SELECT to_jsonb(o) AS j FROM bss_orders__order o WHERE order_id='{}'",
            draft.order
        ))
        .await;
    assert_eq!(
        (row["state"].clone(), row["current_version"].clone()),
        (json!("draft"), json!(1))
    );
    // Same key: the settled refusal replays, even though the day is now "right".
    let engine = t.engine(&[]);
    let mut req = request(draft.order, Trigger::Submit, "roll-1", 1);
    req.expected_draft_revision = Some(2);
    let replay = engine
        .transition(
            &buyer(),
            req,
            &fixed(Prepared::new(
                crate::domain::contributions::AggregateContribution::None,
            )),
        )
        .await
        .unwrap();
    assert!(
        matches!(replay.kind, OutcomeKind::Replayed { .. }),
        "{replay:?}"
    );
    assert_eq!(replay.response, out.response);
    assert_eq!(no_admission(&t, draft.order).await, 0);
    // A fresh attempt resolves every date-dependent input again and commits.
    let fresh = basis(&t, &draft, PreparationClock::Database).await;
    let out = submit(&t, &draft, &fresh, "roll-2").await;
    assert!(matches!(out.kind, OutcomeKind::Committed { .. }), "{out:?}");
    let lines = order_lines(&t, draft.order).await;
    assert_eq!(lines[1].contract_effective_date, fresh.proposed_utc_date.0);
}

/// A policy-required date left unauthored fails the gate even though a default is available,
/// and the refusal stores no admission snapshot.
#[tokio::test]
async fn a_required_unauthored_date_refuses_at_the_gate_without_admission() {
    let t = T::new().await;
    let (policy, _) = t.env.pg.role("policy").await.unwrap();
    promote(
        &policy,
        &plan((false, false), &[(TENANT, false, true)], &[]),
    )
    .await
    .unwrap();
    let draft = authored_draft(&t, "req").await;
    let basis = basis(&t, &draft, PreparationClock::Database).await;
    let undated = draft.lines[1].line_id;
    assert_eq!(basis.failures(), vec![(undated, DateField::AcceptanceDue)]);
    let out = submit(&t, &draft, &basis, "req-1").await;
    let OutcomeKind::Refused { reason, .. } = out.kind else {
        panic!("{out:?}")
    };
    assert_eq!(reason, Reason::DateCascadeInvalid);
    assert_eq!(
        t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__order_line WHERE order_id='{}'",
            draft.order
        ))
        .await,
        0
    );
}
