use serde_json::json;
use time::macros::datetime;
use uuid::Uuid;

use super::{
    Binding, DelegatedPermissions, Grant, GrantState, GrantTerms, Permissions, ResourceRef,
    SubjectRef, TerminationReason, Validity, Window,
};
use crate::ids::{EvidenceId, GrantId};

fn gts(id: &str) -> gts::GtsTypeId {
    serde_json::from_value(json!(id)).unwrap()
}

fn grant() -> Grant {
    Grant {
        id: GrantId(Uuid::nil()),
        holder_tenant_id: Uuid::nil(),
        resource: ResourceRef {
            gts_type: gts("gts.cf.core.lic.res.v1~acme.app.lic.seat.v1~"),
            id: None,
        },
        subject_type: gts("gts.cf.core.lic.subj.v1~cf.core.lic.tenant.v1~"),
        binding: Binding::Subject {
            subject: SubjectRef {
                gts_type: gts("gts.cf.core.lic.subj.v1~cf.core.lic.tenant.v1~"),
                id: "u1".to_owned(),
            },
        },
        binding_fixed: false,
        validity: Validity::Own {
            window: Window {
                not_before: datetime!(2026-01-01 00:00 UTC),
                expires_at: datetime!(2027-01-01 00:00 UTC),
                grace_until: None,
            },
        },
        conditions: None,
        permissions: Permissions {
            may_bind: true,
            max_depth: 1,
            requires_acceptance: false,
        },
        recipient_rules: None,
        parent_grant_id: None,
        delegation_id: None,
        pack_id: None,
        evidence_id: EvidenceId(Uuid::nil()),
        state: GrantState::Active,
        termination_reason: None,
        version: 3,
    }
}

#[test]
fn grant_round_trips_with_tagged_binding_and_rfc3339_window() {
    let json = serde_json::to_value(grant()).unwrap();
    assert_eq!(json["binding"]["kind"], "subject");
    assert_eq!(json["validity"]["kind"], "own");
    assert_eq!(
        json["validity"]["window"]["not_before"],
        "2026-01-01T00:00:00Z"
    );
    assert_eq!(json["state"], "active");
    assert_eq!(serde_json::from_value::<Grant>(json).unwrap(), grant());
}

#[test]
fn inherited_validity_and_delegated_binding_round_trip() {
    let mut g = grant();
    g.validity = Validity::Inherited {
        window: *g.validity.window(),
    };
    g.binding = Binding::Delegated {
        grant_id: GrantId(Uuid::nil()),
    };
    let json = serde_json::to_value(&g).unwrap();
    assert_eq!(json["validity"]["kind"], "inherited");
    assert_eq!(
        json["validity"]["window"]["expires_at"],
        "2027-01-01T00:00:00Z"
    );
    assert_eq!(json["binding"]["kind"], "delegated");
    assert_eq!(serde_json::from_value::<Grant>(json).unwrap(), g);
}

#[test]
fn grant_ignores_unknown_fields() {
    let mut json = serde_json::to_value(grant()).unwrap();
    json["added_later"] = json!(1);
    json["permissions"]["added_later"] = json!(1);
    json["validity"]["window"]["added_later"] = json!(1);
    json["resource"]["added_later"] = json!(1);
    json["binding"]["subject"]["added_later"] = json!(1);
    assert_eq!(serde_json::from_value::<Grant>(json).unwrap(), grant());
}

#[test]
fn request_side_types_reject_unknown_fields() {
    let err = serde_json::from_value::<DelegatedPermissions>(json!({
        "may_bind": true,
        "requires_acceptance": false,
        "may_bindd": true,
    }));
    assert!(err.is_err());
}

#[test]
fn grant_terms_reject_unknown_nested_fields() {
    let terms = json!({
        "resource": { "type": "gts.cf.core.lic.res.v1~acme.app.lic.seat.v1~", "id": "42" },
        "subject_type": "gts.cf.core.lic.subj.v1~cf.core.lic.tenant.v1~",
        "window": {
            "not_before": "2026-01-01T00:00:00Z",
            "expires_at": "2027-01-01T00:00:00Z",
        },
        "permissions": { "may_bind": true, "max_depth": 1, "requires_acceptance": false },
    });
    assert!(serde_json::from_value::<GrantTerms>(terms.clone()).is_ok());
    for (path, key) in [
        ("resource", "instance_id"),
        ("window", "grace_untill"),
        ("permissions", "added_later"),
    ] {
        let mut t = terms.clone();
        t[path][key] = json!("x");
        assert!(
            serde_json::from_value::<GrantTerms>(t).is_err(),
            "{path}.{key}"
        );
    }
}

#[test]
fn grant_reads_unknown_enum_values() {
    let mut json = serde_json::to_value(grant()).unwrap();
    json["state"] = json!("terminated");
    json["termination_reason"] = json!("expired_by_policy");
    json["binding"] = json!({ "kind": "pool", "pool_id": 7 });
    let g = serde_json::from_value::<Grant>(json).unwrap();
    assert_eq!(g.termination_reason, Some(TerminationReason::Unknown));
    assert_eq!(g.binding, Binding::Unknown);
}

#[test]
fn unknown_enum_values_do_not_serialize() {
    assert!(serde_json::to_value(TerminationReason::Unknown).is_err());
    assert!(serde_json::to_value(Binding::Unknown).is_err());
}
