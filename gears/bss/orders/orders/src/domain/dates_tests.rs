use super::*;
use std::sync::Arc;
use time::macros::{date, datetime};

const TENANT: Uuid = Uuid::from_u128(10);

fn row(id: u128, tenant: Option<Uuid>, sa: bool, ad: bool, revision: i64) -> PolicyRow {
    PolicyRow {
        policy_id: Uuid::from_u128(id),
        resource_tenant_id: tenant,
        service_activation_required: sa,
        acceptance_due_required: ad,
        revision,
    }
}
fn default_row(sa: bool, ad: bool) -> PolicyRow {
    row(0x121, None, sa, ad, 1)
}
fn policy(sa: bool, ad: bool) -> DatePolicySnapshot {
    DatePolicySnapshot::effective(TENANT, &[default_row(sa, ad)]).unwrap()
}

#[test]
fn the_tenant_row_wins_and_the_platform_default_is_the_fallback() {
    let default = default_row(false, true);
    let tenant = row(500, Some(TENANT), true, false, 7);
    let snap = DatePolicySnapshot::effective(TENANT, &[default, tenant]).unwrap();
    assert_eq!(
        (
            snap.scope,
            snap.resource_tenant_id,
            snap.policy_id,
            snap.revision
        ),
        (
            PolicyScope::ResourceTenant,
            Some(TENANT),
            Uuid::from_u128(500),
            7
        )
    );
    assert!(snap.service_activation_required && !snap.acceptance_due_required);
    let snap = DatePolicySnapshot::effective(TENANT, &[default]).unwrap();
    assert_eq!(
        (snap.scope, snap.resource_tenant_id, snap.revision),
        (PolicyScope::PlatformDefault, None, 1)
    );
    assert!(!snap.service_activation_required && snap.acceptance_due_required);
    assert!(snap.is_coherent());
    assert_eq!(
        snap.switch_state(),
        serde_json::json!({
            "service_activation_required": false, "acceptance_due_required": true,
            "scope": "platform_default", "resource_tenant_id": null,
            "policy_id": PLATFORM_DEFAULT_POLICY_ID, "revision": 1
        })
    );
}

#[test]
fn a_missing_or_invalid_policy_never_invents_permissive_switches() {
    let tenant = row(500, Some(TENANT), false, false, 3);
    assert_eq!(
        DatePolicySnapshot::effective(TENANT, &[]),
        Err(PolicyFault::Missing)
    );
    // A tenant row alone is not enough: deployment must provide the default.
    assert_eq!(
        DatePolicySnapshot::effective(TENANT, &[tenant]),
        Err(PolicyFault::Missing)
    );
    for rows in [
        vec![row(0x121, None, false, false, 0)],
        vec![
            default_row(false, false),
            row(500, Some(TENANT), false, false, -1),
        ],
        vec![default_row(false, false), default_row(true, true)],
        vec![default_row(false, false), tenant, tenant],
        vec![row(0x999, None, false, false, 1)],
        vec![
            default_row(false, false),
            row(501, Some(Uuid::from_u128(11)), true, true, 2),
        ],
        vec![
            default_row(false, false),
            row(502, Some(Uuid::nil()), true, true, 2),
        ],
        // A tenant row under the platform default's identity is incoherent (as `is_coherent`
        // and the line CHECK both say), not a tenant policy.
        vec![
            default_row(false, false),
            row(0x121, Some(TENANT), true, true, 2),
        ],
    ] {
        assert_eq!(
            DatePolicySnapshot::effective(TENANT, &rows),
            Err(PolicyFault::Invalid),
            "{rows:?}"
        );
    }
}

/// Every switch combination against every supplied/absent combination of the three fields.
#[test]
fn every_switch_and_presence_combination_follows_the_cascade() {
    let proposed = date!(2026 - 10 - 07);
    let (ce, sa, ad) = (
        date!(2026 - 11 - 01),
        date!(2026 - 11 - 15),
        date!(2026 - 10 - 20),
    );
    for (sa_req, ad_req) in [(false, false), (true, false), (false, true), (true, true)] {
        let policy = policy(sa_req, ad_req);
        for mask in 0u8..8 {
            let supplied = SuppliedDates {
                contract_effective: (mask & 1 != 0).then_some(ce),
                service_activation: (mask & 2 != 0).then_some(sa),
                acceptance_due: (mask & 4 != 0).then_some(ad),
            };
            let resolution = resolve_line(supplied, &policy, proposed);
            let mut missing = Vec::new();
            if sa_req && supplied.service_activation.is_none() {
                missing.push(DateField::ServiceActivation);
            }
            if ad_req && supplied.acceptance_due.is_none() {
                missing.push(DateField::AcceptanceDue);
            }
            let case = format!("switches ({sa_req},{ad_req}) mask {mask:03b}");
            if !missing.is_empty() {
                // A default never satisfies a requirement, even though one is available.
                assert_eq!(
                    resolution,
                    LineResolution::Invalid {
                        missing_required: missing
                    },
                    "{case}"
                );
                continue;
            }
            let LineResolution::Resolved { dates } = resolution else {
                panic!("{case}: {resolution:?}")
            };
            let effective = supplied.contract_effective.unwrap_or(proposed);
            assert_eq!(dates.contract_effective.date(), effective, "{case}");
            assert_eq!(
                dates.contract_effective.source,
                if supplied.contract_effective.is_some() {
                    DateSource::Supplied
                } else {
                    DateSource::TransitionDate
                },
                "{case}"
            );
            for (field, value) in [
                (dates.service_activation, supplied.service_activation),
                (dates.acceptance_due, supplied.acceptance_due),
            ] {
                match value {
                    // Supplied values are retained exactly.
                    Some(v) => assert_eq!(
                        (field.date(), field.source),
                        (v, DateSource::Supplied),
                        "{case}"
                    ),
                    // Optional omitted dates default to the resolved contract-effective date.
                    None => assert_eq!(
                        (field.date(), field.source),
                        (effective, DateSource::ContractEffective),
                        "{case}"
                    ),
                }
            }
            assert_eq!(
                dates.uses_transition_date(),
                supplied.contract_effective.is_none(),
                "{case}"
            );
        }
    }
}

#[test]
fn the_basis_reports_every_failing_line_for_the_gate() {
    let ok = Uuid::from_u128(1);
    let bad = Uuid::from_u128(2);
    let basis = DateBasis::resolve(
        TENANT,
        policy(true, true),
        date!(2026 - 10 - 07),
        [
            (
                ok,
                SuppliedDates {
                    contract_effective: None,
                    service_activation: Some(date!(2026 - 10 - 09)),
                    acceptance_due: Some(date!(2026 - 10 - 08)),
                },
            ),
            (bad, SuppliedDates::default()),
        ],
    );
    assert_eq!(
        basis.failures(),
        vec![
            (bad, DateField::ServiceActivation),
            (bad, DateField::AcceptanceDue)
        ]
    );
    assert_eq!(
        basis.line(ok).unwrap().contract_effective.date(),
        date!(2026 - 10 - 07)
    );
    assert!(basis.line(bad).is_none());
}

/// Injected clock across UTC midnight: prepared at 23:59:59.999999 UTC on one day, the engine's
/// transition timestamp falls on the next UTC day. A non-UTC offset is normalized first.
#[test]
fn a_utc_day_rollover_refuses_and_the_same_day_passes() {
    let prepared_at = datetime!(2026-10-07 23:59:59.999_999 UTC);
    let basis = Arc::new(DateBasis::resolve(
        TENANT,
        policy(false, false),
        utc_date(prepared_at),
        [(Uuid::from_u128(1), SuppliedDates::default())],
    ));
    assert_eq!(
        basis.verdict_at(datetime!(2026-10-07 00:00:00 UTC)),
        GuardVerdict::Pass
    );
    assert_eq!(basis.verdict_at(prepared_at), GuardVerdict::Pass);
    assert_eq!(
        basis.verdict_at(datetime!(2026-10-08 00:00:00 UTC)),
        GuardVerdict::Fail(Reason::DateCascadeInvalid)
    );
    // 2026-10-08 01:30 at +02:00 is still 2026-10-07 in UTC.
    assert_eq!(
        basis.verdict_at(datetime!(2026-10-08 01:30 +02:00)),
        GuardVerdict::Pass
    );
    assert_eq!(
        basis.verdict_at(datetime!(2026-10-07 20:00 -04:00)),
        GuardVerdict::Fail(Reason::DateCascadeInvalid)
    );
    // Fully supplied dates still refuse: the run's assessment was evaluated at the stale date.
    let supplied = DateBasis::resolve(
        TENANT,
        policy(false, false),
        date!(2026 - 10 - 07),
        [(
            Uuid::from_u128(1),
            SuppliedDates {
                contract_effective: Some(date!(2026 - 12 - 01)),
                service_activation: Some(date!(2026 - 12 - 01)),
                acceptance_due: Some(date!(2026 - 12 - 01)),
            },
        )],
    );
    assert_eq!(
        supplied.verdict_at(datetime!(2026-10-08 00:00:00 UTC)),
        GuardVerdict::Fail(Reason::DateCascadeInvalid)
    );
}

#[test]
fn the_frozen_basis_round_trips_and_rejects_tampering() {
    let tenant = DatePolicySnapshot::effective(
        TENANT,
        &[
            default_row(false, false),
            row(500, Some(TENANT), true, false, 4),
        ],
    )
    .unwrap();
    let basis = DateBasis::resolve(
        TENANT,
        tenant,
        date!(2026 - 10 - 07),
        [
            (
                Uuid::from_u128(1),
                SuppliedDates {
                    contract_effective: None,
                    service_activation: Some(date!(2026 - 10 - 30)),
                    acceptance_due: None,
                },
            ),
            (Uuid::from_u128(2), SuppliedDates::default()),
        ],
    );
    let frozen = basis.frozen();
    assert_eq!(frozen["proposed_utc_date"], "2026-10-07");
    assert_eq!(frozen["policy"]["scope"], "resource_tenant");
    assert_eq!(DateBasis::from_frozen(&frozen).unwrap(), basis);
    let tamper = |path: &[&str], value: serde_json::Value| {
        let mut v = frozen.clone();
        let mut cursor = &mut v;
        for p in &path[..path.len() - 1] {
            cursor = &mut cursor[*p];
        }
        cursor[path[path.len() - 1]] = value;
        DateBasis::from_frozen(&v)
    };
    let line1 = Uuid::from_u128(1).to_string();
    let line2 = Uuid::from_u128(2).to_string();
    for (path, value) in [
        (vec!["format"], serde_json::json!(2)),
        (vec!["policy", "revision"], serde_json::json!(0)),
        (
            vec!["policy", "scope"],
            serde_json::json!("platform_default"),
        ),
        (vec!["policy", "unknown"], serde_json::json!(true)),
        // A defaulted date that does not follow from the proposed date.
        (
            vec!["lines", &line1, "dates", "contract_effective", "value"],
            serde_json::json!("2026-10-06"),
        ),
        // A required field claimed as defaulted.
        (
            vec!["lines", &line1, "dates", "service_activation", "source"],
            serde_json::json!("contract_effective"),
        ),
        // A missing field the snapshot does not require.
        (
            vec!["lines", &line2, "missing_required"],
            serde_json::json!(["service_activation", "acceptance_due"]),
        ),
        (
            vec!["lines", &line2, "missing_required"],
            serde_json::json!(["service_activation", "service_activation"]),
        ),
        // The basis is bound to the resource tenant whose policy it snapshotted.
        (
            vec!["resource_tenant_id"],
            serde_json::json!(Uuid::from_u128(11)),
        ),
        (vec!["resource_tenant_id"], serde_json::json!(Uuid::nil())),
    ] {
        assert_eq!(tamper(&path, value), Err(InvalidBasis), "{path:?}");
    }
}
