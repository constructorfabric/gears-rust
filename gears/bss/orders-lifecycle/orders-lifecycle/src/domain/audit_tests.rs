//! Runtime audit encoder against the frozen independent S1-06/S5-01 vectors (Python-authored
//! preimages **and** SHA-256 digests), plus mutation, boundary, writer-shape and chain tests.
#![allow(clippy::unwrap_used, clippy::expect_used)] // Corrupt frozen fixtures must stop the test.
use super::*;
use bss_orders_lifecycle_sdk::catalog::{OrderState, Reason, Trigger};
use serde_json::{Value, json};
use toolkit_security::SecurityContext;

fn vectors() -> Vec<Value> {
    serde_json::from_str::<Value>(include_str!("../../tests/fixtures/conformance-v1.json")).unwrap()
        ["vectors"]
        .as_array()
        .unwrap()
        .clone()
}
fn named(id: &str) -> Value {
    vectors().into_iter().find(|v| v["id"] == id).unwrap()
}
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, b| {
        write!(out, "{b:02x}").unwrap();
        out
    })
}
type Mutation = (&'static str, Box<dyn Fn(&mut AuditRow)>);
/// A deterministic 32-byte test key (D-204); never a deployment value.
fn test_key(id: &str, fill: u8) -> AdminTextKey {
    AdminTextKey::new(id, &[fill; MIN_ADMIN_TEXT_KEY_BYTES]).unwrap()
}
fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
fn uuid_of(v: &Value) -> Option<Uuid> {
    v.as_str().map(|s| Uuid::parse_str(s).unwrap())
}
fn text_of(v: &Value) -> Option<String> {
    v.as_str().map(str::to_owned)
}
fn int_of(v: &Value) -> Option<i64> {
    v.as_str().map(|s| s.parse().unwrap())
}
fn instant(micros: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(micros) * 1_000).unwrap()
}
/// The fixture's string-typed row, converted to the stored typed row.
fn row_of(input: &Value) -> AuditRow {
    AuditRow {
        hash_version: i16::try_from(input["hash_version"].as_i64().unwrap()).unwrap(),
        audit_id: uuid_of(&input["audit_id"]).unwrap(),
        audit_tenant_id: uuid_of(&input["audit_tenant_id"]),
        subject_tenant_id: uuid_of(&input["subject_tenant_id"]).unwrap(),
        resource_tenant_id: uuid_of(&input["resource_tenant_id"]),
        order_id: uuid_of(&input["order_id"]),
        requested_order_ref: uuid_of(&input["requested_order_ref"]),
        sequence: int_of(&input["sequence"]),
        from_state: text_of(&input["from_state"]),
        to_state: text_of(&input["to_state"]),
        trigger: text_of(&input["trigger"]).unwrap(),
        outcome: text_of(&input["outcome"]).unwrap(),
        actor: text_of(&input["actor"]).unwrap(),
        actor_class: text_of(&input["actor_class"]).unwrap(),
        delegation_proof_ref: text_of(&input["delegation_proof_ref"]),
        reason: text_of(&input["reason"]).unwrap(),
        changed_field: text_of(&input["changed_field"]),
        prior_value: text_of(&input["prior_value"]),
        new_value: text_of(&input["new_value"]),
        idempotency_key: text_of(&input["idempotency_key"]).unwrap(),
        correlation_id: uuid_of(&input["correlation_id"]),
        version: int_of(&input["version"]).map(|v| i32::try_from(v).unwrap()),
        created_at: instant(int_of(&input["created_at"]).unwrap()),
        prev_hash: input["prev_hash"].as_str().map(unhex),
        caller_reason: text_of(&input["caller_reason"]),
        force_request_observation: input
            .get("force_request_observation")
            .filter(|v| !v.is_null())
            .map(|o| ForceRequestObservation {
                audit_sequence: int_of(&o["audit_sequence"]).unwrap(),
                state: text_of(&o["state"]).unwrap(),
                version: i32::try_from(int_of(&o["version"]).unwrap()).unwrap(),
            }),
    }
}
fn checkpoint_of(input: &Value) -> (CheckpointHeader, Vec<CheckpointMember>) {
    let header = CheckpointHeader {
        format_version: i16::try_from(input["format_version"].as_i64().unwrap()).unwrap(),
        audit_tenant_id: uuid_of(&input["audit_tenant_id"]).unwrap(),
        checkpoint_sequence: int_of(&input["checkpoint_sequence"]).unwrap(),
        captured_at: instant(int_of(&input["captured_at"]).unwrap()),
        member_count: int_of(&input["member_count"]).unwrap(),
        prev_checkpoint_hash: unhex(input["prev_checkpoint_hash"].as_str().unwrap()),
    };
    let members = input["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| CheckpointMember {
            order_id: uuid_of(&m["order_id"]).unwrap(),
            audit_sequence: int_of(&m["audit_sequence"]).unwrap(),
            entry_hash: unhex(m["entry_hash"].as_str().unwrap()),
        })
        .collect();
    (header, members)
}
fn sha(bytes: &[u8]) -> String {
    hex(&super::sha256(bytes))
}

#[test]
fn runtime_encoder_reproduces_every_frozen_independent_vector() {
    let mut audited = 0;
    for vector in vectors() {
        let input = &vector["input"];
        let bytes = match vector["kind"].as_str().unwrap() {
            "audit" => {
                audited += 1;
                preimage(&row_of(input)).unwrap()
            }
            "genesis" => genesis_preimage(
                uuid_of(&input["audit_tenant_id"]).unwrap(),
                uuid_of(&input["order_id"]).unwrap(),
            ),
            "checkpoint" => {
                let (header, members) = checkpoint_of(input);
                let bytes = checkpoint_preimage(&header, &members).unwrap();
                assert_eq!(
                    hex(&checkpoint_hash(&header, &members).unwrap()),
                    vector["sha256"]
                );
                bytes
            }
            "checkpoint_genesis" => {
                let digest = checkpoint_genesis(uuid_of(&input["audit_tenant_id"]).unwrap());
                assert_eq!(hex(&digest), vector["sha256"], "{}", vector["id"]);
                continue;
            }
            _ => continue, // request/cursor/framing profiles belong to other packages
        };
        assert_eq!(hex(&bytes), vector["preimage_hex"], "{}", vector["id"]);
        assert_eq!(sha(&bytes), vector["sha256"], "{}", vector["id"]);
    }
    assert_eq!(audited, 20, "all v1/v2/v3 audit vectors are exercised");
    let g = named("order-genesis");
    assert_eq!(
        hex(&genesis(
            uuid_of(&g["input"]["audit_tenant_id"]).unwrap(),
            uuid_of(&g["input"]["order_id"]).unwrap()
        )),
        g["sha256"]
    );
}

#[test]
fn every_vector_row_with_a_valid_db_shape_verifies_and_the_empty_caller_reason_does_not() {
    for vector in vectors().into_iter().filter(|v| v["kind"] == "audit") {
        let row = row_of(&vector["input"]);
        let stored = unhex(vector["sha256"].as_str().unwrap());
        let result = verify_entry(&row, &stored);
        let id = vector["id"].as_str().unwrap();
        if id == "v2-caller-empty" {
            // Byte fixture only: an empty caller_reason is encodable but violates the 1..=4096
            // row-shape CHECK, so it can never be stored or verified as a row.
            assert!(matches!(result, Err(VerifyError::Malformed { .. })), "{id}");
        } else {
            // Resolved refusals keep their observed state (to_state = from_state, §3.7/D-98);
            // the corrected vectors verify as stored rows like every other audit vector.
            if id.ends_with("-resolved-refusal") && !id.contains("unresolved") {
                assert_eq!(row.to_state, row.from_state, "{id}");
            }
            assert_eq!(hex(&result.unwrap()), vector["sha256"], "{}", vector["id"]);
        }
    }
}

/// Mutating any covered field changes the digest or is refused (never silently ignored).
#[test]
fn every_covered_field_changes_the_digest_or_is_refused() {
    for id in [
        "v3-force-request",
        "v3-after-v2",
        "v2-after-v1-caller-reason",
        "v1-create",
    ] {
        let base = row_of(&named(id)["input"]);
        let original = preimage(&base).unwrap();
        let other = Uuid::from_u128(0xfeed);
        let mutations: Vec<Mutation> = vec![
            (
                "hash_version",
                Box::new(|r| r.hash_version = if r.hash_version == 3 { 2 } else { 3 }),
            ),
            ("audit_id", Box::new(move |r| r.audit_id = other)),
            (
                "audit_tenant_id",
                Box::new(move |r| {
                    r.audit_tenant_id = r.audit_tenant_id.map_or(Some(other), |_| None);
                }),
            ),
            (
                "subject_tenant_id",
                Box::new(move |r| r.subject_tenant_id = other),
            ),
            (
                "resource_tenant_id",
                Box::new(move |r| {
                    r.resource_tenant_id = r.resource_tenant_id.map_or(Some(other), |_| None);
                }),
            ),
            (
                "order_id",
                Box::new(move |r| r.order_id = r.order_id.map_or(Some(other), |_| None)),
            ),
            (
                "requested_order_ref",
                Box::new(move |r| {
                    r.requested_order_ref = r.requested_order_ref.map_or(Some(other), |_| None);
                }),
            ),
            (
                "sequence",
                Box::new(|r| r.sequence = r.sequence.map_or(Some(9), |s| Some(s + 1))),
            ),
            (
                "from_state",
                Box::new(|r| {
                    r.from_state = r.from_state.as_ref().map_or(Some("draft".into()), |_| None);
                }),
            ),
            (
                "to_state",
                Box::new(|r| r.to_state = Some("expired".into())),
            ),
            ("trigger", Box::new(|r| r.trigger.push('x'))),
            (
                "outcome",
                Box::new(|r| {
                    r.outcome = if r.outcome == "committed" {
                        "refused".into()
                    } else {
                        "committed".into()
                    }
                }),
            ),
            (
                "actor",
                Box::new(|r| r.actor = Uuid::from_u128(7).to_string()),
            ),
            (
                "actor_class",
                Box::new(|r| r.actor_class = "service".into()),
            ),
            (
                "delegation_proof_ref",
                Box::new(|r| r.delegation_proof_ref = Some("proof-1".into())),
            ),
            ("reason", Box::new(|r| r.reason.push('x'))),
            (
                "changed_field",
                Box::new(|r| r.changed_field = Some("display_label".into())),
            ),
            (
                "prior_value",
                Box::new(|r| r.prior_value = Some(String::new())),
            ),
            ("new_value", Box::new(|r| r.new_value = Some(String::new()))),
            (
                "idempotency_key",
                Box::new(|r| r.idempotency_key.pop().map_or((), |_| ())),
            ),
            ("correlation_id", Box::new(|r| r.correlation_id = None)),
            (
                "version",
                Box::new(|r| r.version = r.version.map_or(Some(3), |v| Some(v + 1))),
            ),
            (
                "created_at",
                Box::new(|r| r.created_at += time::Duration::microseconds(1)),
            ),
            (
                "prev_hash",
                Box::new(|r| {
                    r.prev_hash = Some(r.prev_hash.as_ref().map_or(vec![1; 32], |h| {
                        let mut h = h.clone();
                        h[31] ^= 1;
                        h
                    }));
                }),
            ),
            (
                "caller_reason",
                Box::new(|r| {
                    r.caller_reason = r
                        .caller_reason
                        .as_ref()
                        .map_or(Some(String::new()), |_| None);
                }),
            ),
            (
                "force_request_observation",
                Box::new(|r| {
                    r.force_request_observation = r.force_request_observation.as_ref().map_or(
                        Some(ForceRequestObservation {
                            audit_sequence: 1,
                            state: "draft".into(),
                            version: 1,
                        }),
                        |o| {
                            Some(ForceRequestObservation {
                                audit_sequence: o.audit_sequence + 1,
                                ..o.clone()
                            })
                        },
                    );
                }),
            ),
        ];
        assert_eq!(mutations.len(), 26, "one mutation per covered column");
        for (field, mutate) in &mutations {
            let mut changed = base.clone();
            mutate(&mut changed);
            if let Ok(bytes) = preimage(&changed) {
                assert_ne!(bytes, original, "{id}: uncovered field {field}");
            }
        }
    }
}

#[test]
fn unknown_versions_and_uncoverable_evidence_refuse_without_fallback() {
    let base = row_of(&named("v3-create")["input"]);
    for version in [0, 4, -1, i16::MAX] {
        let mut bad = base.clone();
        bad.hash_version = version;
        assert_eq!(preimage(&bad), Err(AuditError::UnsupportedVersion(version)));
        assert!(matches!(
            verify_entry(&bad, &[0; 32]),
            Err(VerifyError::UnsupportedVersion { version: v, .. }) if v == version
        ));
    }
    let mut v1 = row_of(&named("v2-after-v1-caller-reason")["input"]);
    v1.hash_version = 1;
    assert_eq!(preimage(&v1), Err(AuditError::Uncovered("caller_reason")));
    let mut v2 = row_of(&named("v3-force-request")["input"]);
    v2.hash_version = 2;
    assert_eq!(
        preimage(&v2),
        Err(AuditError::Uncovered("force_request_observation"))
    );
    // Malformed digests and numbers refuse rather than truncate.
    let mut short = base.clone();
    short.prev_hash = Some(vec![0; 31]);
    assert_eq!(preimage(&short), Err(AuditError::DigestLength("prev_hash")));
    let mut zero = base.clone();
    zero.sequence = Some(0);
    assert_eq!(preimage(&zero), Err(AuditError::NonPositive("sequence")));
    let mut negative = base.clone();
    negative.version = Some(-1);
    assert_eq!(preimage(&negative), Err(AuditError::NonPositive("version")));
    assert!(matches!(
        verify_entry(&base, &[0; 31]),
        Err(VerifyError::HashLength { .. })
    ));
}

#[test]
fn null_empty_adjacent_boundaries_and_signed_microsecond_instants_are_exact() {
    // NULL is 0x00; empty is 0x01 00000000; adjacent fields never merge.
    let mut f = Frames::default();
    f.text("a", None).unwrap();
    f.text("b", Some("")).unwrap();
    assert_eq!(f.finish(), vec![0, 1, 0, 0, 0, 0]);
    let caller = |value: Option<&str>| {
        let mut row = row_of(&named("v2-caller-null")["input"]);
        row.caller_reason = value.map(str::to_owned);
        preimage(&row).unwrap()
    };
    assert_ne!(caller(None), caller(Some("")));
    let split = |a: &str, b: &str| {
        let mut row = row_of(&named("v3-create")["input"]);
        row.idempotency_key = a.into();
        row.delegation_proof_ref = Some(b.into());
        preimage(&row).unwrap()
    };
    assert_ne!(split("ab", "c"), split("a", "bc"));
    // Signed big-endian microseconds; pre-epoch and the epoch itself.
    for (value, bytes) in [
        (-1_i64, [0xff; 8]),
        (0, [0; 8]),
        (1, [0, 0, 0, 0, 0, 0, 0, 1]),
    ] {
        assert_eq!(micros(instant(value)).unwrap().to_be_bytes(), bytes);
    }
    // Sub-microsecond residue is never hashed; normalization floors (also before the epoch).
    let ragged = instant(5) + time::Duration::nanoseconds(999);
    assert_eq!(micros(ragged), Err(AuditError::Instant));
    assert_eq!(normalize_instant(ragged).unwrap(), instant(5));
    let before = instant(-5) - time::Duration::nanoseconds(1);
    assert_eq!(normalize_instant(before).unwrap(), instant(-6));
    let offset = instant(42).to_offset(time::UtcOffset::from_hms(5, 30, 0).unwrap());
    assert_eq!(
        micros(offset).unwrap(),
        42,
        "the offset never contributes bytes"
    );
    // UUIDs are 16 network-order bytes, never text.
    let mut f = Frames::default();
    f.uuid("id", Some(Uuid::from_u128(0x0102))).unwrap();
    let out = f.finish();
    assert_eq!(&out[..5], &[1, 0, 0, 0, 16]);
    assert_eq!(&out[19..], &[1, 2]);
    // Positive bounds of the signed columns.
    let mut row = row_of(&named("v3-later-resource-change")["input"]);
    row.sequence = Some(i64::MAX);
    row.version = Some(i32::MAX);
    assert!(preimage(&row).is_ok());
}

// ------------------------------------------------------------------------------------------------
// Writer shapes

fn u(n: u128) -> Uuid {
    Uuid::from_u128(n)
}
fn ctx(subject: u128, tenant: u128) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(u(subject))
        .subject_tenant_id(u(tenant))
        .build()
        .unwrap()
}
fn identities() -> ActorIdentities {
    ActorIdentities::new(
        Some(Principal {
            subject_id: u(900),
            subject_tenant_id: u(1),
        }),
        [
            (
                ServiceRole::Workflow,
                Principal {
                    subject_id: u(901),
                    subject_tenant_id: u(1),
                },
            ),
            (
                ServiceRole::Subscriptions,
                Principal {
                    subject_id: u(902),
                    subject_tenant_id: u(1),
                },
            ),
        ],
    )
    .unwrap()
}
fn key() -> IdempotencyKey {
    IdempotencyKey::try_from("key-1".to_owned()).unwrap()
}
fn evidence(proof: Option<&str>) -> AttemptEvidence {
    let proof = proof.map(|p| DelegationProofRef::try_from(p.to_owned()).unwrap());
    AttemptEvidence::new(
        identities().classify(&ctx(30, 20)).unwrap(),
        proof.as_ref(),
        &key(),
        Some(u(40)),
        instant(1_791_288_000_123_456) + time::Duration::nanoseconds(789),
    )
    .unwrap()
}
fn facts(state: OrderState, version: i32, sequence: i64) -> OrderFacts {
    OrderFacts {
        order_id: u(100),
        audit_tenant_id: u(10),
        resource_tenant_id: u(10),
        state,
        version,
        audit_sequence: sequence,
    }
}

#[test]
fn actor_class_derives_only_from_configured_authenticated_identities() {
    let ids = identities();
    assert_eq!(
        ids.classify(&ctx(900, 1)).unwrap().class(),
        AuditActorClass::System
    );
    assert_eq!(
        ids.classify(&ctx(901, 1)).unwrap().class(),
        AuditActorClass::Service
    );
    assert_eq!(
        ids.classify(&ctx(902, 1)).unwrap().class(),
        AuditActorClass::Service
    );
    assert_eq!(
        ids.classify(&ctx(30, 20)).unwrap().class(),
        AuditActorClass::User
    );
    // A lookalike subject in another tenant is an ordinary user, not the service.
    assert_eq!(
        ids.classify(&ctx(901, 2)).unwrap().class(),
        AuditActorClass::User
    );
    assert_eq!(
        ids.classify(&ctx(900, 2)).unwrap().class(),
        AuditActorClass::User
    );
    assert_eq!(ids.service_role(&ctx(901, 1)), Some(ServiceRole::Workflow));
    assert_eq!(ids.service_role(&ctx(901, 2)), None);
    let anonymous = SecurityContext::anonymous();
    assert_eq!(ids.classify(&anonymous), Err(AuditError::UnstableActor));
    let actor = ids.classify(&ctx(0xABCD, 20)).unwrap();
    assert_eq!(actor.reference(), "00000000-0000-0000-0000-00000000abcd");
    let p = |s, t| Principal {
        subject_id: u(s),
        subject_tenant_id: u(t),
    };
    for bad in [
        ActorIdentities::new(None, [(ServiceRole::Billing, p(1, 1))]),
        ActorIdentities::new(
            None,
            [
                (ServiceRole::Workflow, p(1, 1)),
                (ServiceRole::Billing, p(1, 1)),
            ],
        ),
        ActorIdentities::new(Some(p(1, 1)), [(ServiceRole::Workflow, p(1, 1))]),
        ActorIdentities::new(None, [(ServiceRole::Workflow, p(0, 1))]),
        ActorIdentities::new(Some(p(2, 0)), [(ServiceRole::Workflow, p(1, 1))]),
    ] {
        assert!(matches!(bad, Err(AuditError::Identities(_))));
    }
}

#[test]
fn committed_create_is_sequence_one_from_genesis_with_a_frozen_namespace() {
    let created = facts(OrderState::Draft, 1, 0);
    let pending = evidence(None).committed_create(u(1), &created).unwrap();
    let sealed = pending.seal(1, genesis(u(10), u(100))).unwrap();
    let row = sealed.row();
    assert_eq!(row.hash_version, 3);
    assert_eq!(
        (
            row.sequence,
            row.from_state.as_deref(),
            row.to_state.as_deref()
        ),
        (Some(1), None, Some("draft"))
    );
    assert_eq!(
        (
            row.trigger.as_str(),
            row.reason.as_str(),
            row.outcome.as_str()
        ),
        ("create", "create", "committed")
    );
    assert_eq!(row.requested_order_ref, None);
    assert_eq!(
        (row.audit_tenant_id, row.resource_tenant_id, row.order_id),
        (Some(u(10)), Some(u(10)), Some(u(100)))
    );
    assert_eq!(
        row.subject_tenant_id,
        u(20),
        "actor home tenant, never order tenancy"
    );
    assert_eq!(row.actor, u(30).to_string());
    assert_eq!(row.actor_class, "user");
    assert_eq!(
        micros(row.created_at).unwrap(),
        1_791_288_000_123_456,
        "normalized once"
    );
    assert_eq!(row.caller_reason, None);
    assert_eq!(row.force_request_observation, None);
    assert_eq!(
        verify_entry(row, &sealed.entry_hash()).unwrap(),
        sealed.entry_hash()
    );
    // A non-fresh aggregate or one whose namespace differs from its resource tenant refuses.
    for bad in [
        facts(OrderState::Draft, 1, 1),
        facts(OrderState::Submitted, 1, 0),
        facts(OrderState::Draft, 2, 0),
        OrderFacts {
            resource_tenant_id: u(11),
            ..created
        },
    ] {
        assert!(evidence(None).committed_create(u(1), &bad).is_err());
    }
}

#[test]
fn committed_transitions_record_before_after_and_only_permitted_caller_text() {
    let before = facts(OrderState::Draft, 1, 1);
    let after = OrderFacts {
        resource_tenant_id: u(11),
        state: OrderState::Cancelled,
        ..before
    };
    let cancel = AuditTrigger::Public(Trigger::Cancel);
    let row = evidence(None)
        .committed_transition(
            u(2),
            cancel,
            &before,
            &after,
            Some("Requested cancellation \u{2014} caf\u{e9}"),
        )
        .unwrap()
        .seal(2, [7; 32])
        .unwrap();
    let r = row.row();
    assert_eq!(
        (r.from_state.as_deref(), r.to_state.as_deref()),
        (Some("draft"), Some("cancelled"))
    );
    assert_eq!(
        r.resource_tenant_id,
        Some(u(11)),
        "current resource tenant snapshot"
    );
    assert_eq!(r.audit_tenant_id, Some(u(10)), "namespace frozen at create");
    assert_eq!(r.requested_order_ref, Some(u(100)));
    assert_eq!(
        r.caller_reason.as_deref(),
        Some("Requested cancellation \u{2014} caf\u{e9}")
    );
    assert_eq!(r.reason, "cancel");
    // Mandatory text missing, text on a trigger without it, invalid text.
    assert!(
        evidence(None)
            .committed_transition(u(2), cancel, &before, &after, None)
            .is_err()
    );
    let submit = AuditTrigger::Public(Trigger::Submit);
    assert_eq!(
        evidence(None).committed_transition(u(2), submit, &before, &after, Some("x")),
        Err(AuditError::NotMinimized("caller_reason"))
    );
    for bad in [String::new(), "a\u{0}b".into(), "x".repeat(4097)] {
        assert!(
            evidence(None)
                .committed_transition(u(2), cancel, &before, &after, Some(&bad))
                .is_err()
        );
    }
    let hold = AuditTrigger::Public(Trigger::Hold);
    let on_hold = OrderFacts {
        state: OrderState::OnHold,
        ..before
    };
    assert!(
        evidence(None)
            .committed_transition(u(2), hold, &before, &on_hold, None)
            .is_ok()
    );
    assert!(
        evidence(None)
            .committed_transition(u(2), hold, &before, &on_hold, Some(&"\u{e9}".repeat(4096)))
            .is_ok()
    );
    // Create/admin shapes and cross-aggregate/namespace mixes refuse.
    for trigger in [Trigger::Create, Trigger::AdministrativeEdit] {
        assert!(
            evidence(None)
                .committed_transition(u(2), AuditTrigger::Public(trigger), &before, &after, None)
                .is_err()
        );
    }
    let other = OrderFacts {
        audit_tenant_id: u(99),
        ..after
    };
    assert!(
        evidence(None)
            .committed_transition(u(2), cancel, &before, &other, Some("x"))
            .is_err()
    );
    // The internal D-201 writer stays in_fulfillment.
    let f = facts(OrderState::InFulfillment, 3, 7);
    let rebuild = evidence(None)
        .committed_transition(u(3), AuditTrigger::ReplaceFulfillmentGrant, &f, &f, None)
        .unwrap()
        .seal(8, [1; 32])
        .unwrap();
    assert_eq!(rebuild.row().trigger, "replace-fulfillment-grant");
    let drifted = OrderFacts {
        state: OrderState::OnHold,
        ..f
    };
    assert!(
        evidence(None)
            .committed_transition(
                u(3),
                AuditTrigger::ReplaceFulfillmentGrant,
                &f,
                &drifted,
                None
            )
            .unwrap()
            .seal(8, [1; 32])
            .is_err()
    );
}

#[test]
fn acknowledged_failure_records_only_a_closed_failure_reason() {
    // 06 §4.4 (D-136): the failed acknowledgement's caller_reason is the closed failure_reason
    // value, never free text, and never the force-fail-only `operator-forced-unreconciled`.
    let before = facts(OrderState::InFulfillment, 2, 5);
    let after = OrderFacts {
        state: OrderState::FulfillmentFailed,
        ..before
    };
    let ack = AuditTrigger::Public(Trigger::AcknowledgeFailed);
    for value in crate::domain::audit::ACKNOWLEDGED_FAILURE_REASONS {
        let sealed = evidence(None)
            .committed_transition(u(4), ack, &before, &after, Some(value))
            .unwrap()
            .seal(6, [3; 32])
            .unwrap();
        assert_eq!(sealed.row().caller_reason.as_deref(), Some(value));
    }
    for bad in [
        "operator-forced-unreconciled",
        "Market-Divergence",
        "market-divergence ",
        "compensation failed, see ticket",
    ] {
        assert_eq!(
            evidence(None).committed_transition(u(4), ack, &before, &after, Some(bad)),
            Err(AuditError::NotMinimized("caller_reason")),
            "{bad}"
        );
    }
    assert_eq!(
        evidence(None).committed_transition(u(4), ack, &before, &after, None),
        Err(AuditError::Shape("caller_reason required"))
    );
    // The forced exit records the approver's free-text reason instead.
    let forced = AuditTrigger::Public(Trigger::ForceFailUnreconciled);
    assert!(
        evidence(None)
            .committed_transition(
                u(4),
                forced,
                &before,
                &after,
                Some("vendor outage, see INC-7")
            )
            .is_ok()
    );
}

#[test]
fn administrative_edits_write_one_minimized_entry_per_changed_allowlisted_field() {
    let order = facts(OrderState::Approved, 2, 4);
    let line = u(555);
    let changes = vec![
        AdminChange {
            field: AdminField::Order(AdminAttribute::ExternalReference),
            prior: None,
            new: Some("PO-2026-001".into()),
        },
        AdminChange {
            field: AdminField::Line(line, AdminAttribute::DisplayLabel),
            prior: Some("Rack A".into()),
            new: Some("rack a".into()),
        },
        AdminChange {
            field: AdminField::Order(AdminAttribute::InternalNotes),
            prior: Some("same".into()),
            new: Some("same".into()),
        },
        AdminChange {
            field: AdminField::Order(AdminAttribute::InternalNotes),
            prior: Some("note".into()),
            new: Some("note ".into()),
        },
    ];
    let mut next = 10;
    let rows = evidence(None)
        .administrative_edits(&test_key("k1", 7), &order, &changes, || {
            next += 1;
            u(next)
        })
        .unwrap();
    assert_eq!(
        rows.len(),
        3,
        "unchanged fields beside changed ones produce no entry"
    );
    let sealed: Vec<_> = rows
        .into_iter()
        .enumerate()
        .map(|(i, p)| p.seal(5 + i64::try_from(i).unwrap(), [2; 32]).unwrap())
        .collect();
    let r0 = sealed[0].row();
    assert_eq!(r0.changed_field.as_deref(), Some("external_reference"));
    assert_eq!(
        (r0.prior_value.as_deref(), r0.new_value.as_deref()),
        (None, Some("PO-2026-001"))
    );
    let r1 = sealed[1].row();
    assert_eq!(
        r1.changed_field,
        Some(format!("lines/{line}/display_label"))
    );
    // Case-only and whitespace-only changes stay distinct after minimization (prior <> new).
    assert_ne!(r1.prior_value, r1.new_value);
    assert_eq!(
        r1.prior_value,
        Some(minimized_text_digest(&test_key("k1", 7), "Rack A"))
    );
    let r2 = sealed[2].row();
    assert_ne!(r2.prior_value, r2.new_value);
    for row in sealed.iter().map(SealedAudit::row) {
        assert_eq!(
            (row.trigger.as_str(), row.reason.as_str()),
            ("administrative-edit", "administrative-edit")
        );
        assert_eq!(row.from_state, row.to_state);
        assert_eq!(row.version, Some(2), "no version bump");
        for value in [&row.prior_value, &row.new_value].into_iter().flatten() {
            assert!(
                !value.contains("Rack") && !value.contains("note"),
                "free text is never copied"
            );
        }
    }
    let x = minimized_text_digest(&test_key("k1", 7), "x");
    assert!(x.starts_with("hmac-sha256:v1:k1:") && x.len() == "hmac-sha256:v1:k1:".len() + 64);
    // Nothing changed, a field outside the allowlist, an unminimizable reference.
    assert!(
        evidence(None)
            .administrative_edits(&test_key("k1", 7), &order, &changes[2..3], Uuid::new_v4)
            .is_err()
    );
    assert!(
        evidence(None)
            .administrative_edits(&test_key("k1", 7), &order, &[], Uuid::new_v4)
            .is_err()
    );
    assert_eq!(
        AdminAttribute::from_token("customer_email"),
        Err(AuditError::NotMinimized("changed_field"))
    );
    for bad in [
        "",
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2ln",
        "ref\nx",
        &"r".repeat(1025),
    ] {
        let change = AdminChange {
            field: AdminField::Order(AdminAttribute::ExternalReference),
            prior: None,
            new: Some(bad.to_owned()),
        };
        assert!(
            evidence(None)
                .administrative_edits(&test_key("k1", 7), &order, &[change], Uuid::new_v4)
                .is_err(),
            "{bad:?}"
        );
    }
}

#[test]
fn resolved_and_unresolved_refusals_have_their_exact_shapes() {
    let order = facts(OrderState::Submitted, 2, 3);
    let submit = AuditTrigger::Public(Trigger::Submit);
    let resolved = evidence(None)
        .resolved_refusal(u(4), submit, &order, Reason::NotAdmissible)
        .unwrap();
    let r = resolved.row();
    assert_eq!(
        (r.outcome.as_str(), r.reason.as_str()),
        ("refused", "not-admissible")
    );
    assert_eq!(
        (r.sequence, r.prev_hash.as_ref(), r.caller_reason.as_ref()),
        (None, None, None)
    );
    assert_eq!(
        (r.from_state.as_deref(), r.to_state.as_deref(), r.version),
        (Some("submitted"), Some("submitted"), Some(2))
    );
    assert_eq!(
        (r.order_id, r.requested_order_ref),
        (Some(u(100)), Some(u(100)))
    );
    assert_eq!(r.force_request_observation, None);

    let unresolved = evidence(Some("proof:abc"))
        .unresolved_refusal(u(5), submit, Some(u(100)), Reason::OrderNotFound)
        .unwrap();
    let r = unresolved.row();
    assert_eq!(
        (r.order_id, r.audit_tenant_id, r.resource_tenant_id),
        (None, None, None)
    );
    assert_eq!(
        (r.from_state.as_ref(), r.to_state.as_ref(), r.version),
        (None, None, None)
    );
    assert_eq!(
        r.requested_order_ref,
        Some(u(100)),
        "validated target identifier retained"
    );
    assert_eq!(r.subject_tenant_id, u(20));
    assert_eq!(r.delegation_proof_ref.as_deref(), Some("proof:abc"));
    // Refused create may have no identifier; any other trigger must keep it.
    let create = AuditTrigger::Public(Trigger::Create);
    assert!(
        evidence(None)
            .unresolved_refusal(u(6), create, None, Reason::NotAdmissible)
            .is_ok()
    );
    assert!(
        evidence(None)
            .unresolved_refusal(u(6), submit, None, Reason::OrderNotFound)
            .is_err()
    );

    // D-201 force request: observation of the exact committed sequence/state/version.
    let held = facts(OrderState::InFulfillment, 7, 55);
    let force = AuditTrigger::Public(Trigger::ForceFailUnreconciled);
    let request = evidence(None)
        .resolved_refusal(u(7), force, &held, Reason::SecondApproverRequired)
        .unwrap();
    assert_eq!(
        request.row().force_request_observation,
        Some(ForceRequestObservation {
            audit_sequence: 55,
            state: "in_fulfillment".into(),
            version: 7
        })
    );
    assert_eq!(
        request.row().sequence,
        None,
        "observation is not a chain sequence"
    );
    assert_eq!(
        ForceRequestObservation::from_json(
            &request
                .row()
                .force_request_observation
                .as_ref()
                .unwrap()
                .to_json()
        )
        .unwrap(),
        request.row().force_request_observation.clone().unwrap()
    );
    for bad in [
        json!({}),
        json!({"audit_sequence":1,"state":"draft"}),
        json!({"audit_sequence":1,"state":"draft","version":1,"x":1}),
        json!({"audit_sequence":"1","state":"draft","version":1}),
    ] {
        assert!(ForceRequestObservation::from_json(&bad).is_err());
    }
    // Other force refusals carry no observation; an aggregate without a committed head cannot.
    let other = evidence(None)
        .resolved_refusal(u(8), force, &held, Reason::NotAdmissible)
        .unwrap();
    assert_eq!(other.row().force_request_observation, None);
    assert!(
        evidence(None)
            .resolved_refusal(
                u(9),
                force,
                &facts(OrderState::InFulfillment, 7, 0),
                Reason::SecondApproverRequired
            )
            .is_err()
    );
}

#[test]
fn proof_references_never_carry_credentials() {
    for credential in [
        "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJ4In0.sig",
        "eyJhbGciOiJSUzI1NiJ9.eyJ4IjoxfQ.a.b.c",
        "-----BEGIN-PRIVATE-KEY-----",
        "https://user:pass@issuer.example/proof/1",
        "proof?token=abc",
        "ref;Signature=xyz",
        "x&api_key=1",
    ] {
        assert!(carries_credential(credential), "{credential}");
        let proof = DelegationProofRef::try_from(credential.to_owned()).unwrap();
        assert_eq!(
            validate_proof_reference(&proof),
            Err(AuditError::CredentialInReference)
        );
        assert!(
            AttemptEvidence::new(
                identities().classify(&ctx(1, 2)).unwrap(),
                Some(&proof),
                &key(),
                None,
                instant(0)
            )
            .is_err()
        );
    }
    for reference in [
        "proof:abc",
        "urn:am:delegation:123e4567",
        "https://issuer.example/proofs/42",
        "a.b.c",
    ] {
        assert!(!carries_credential(reference), "{reference}");
    }
}

// ------------------------------------------------------------------------------------------------
// Chain verification

fn chain(len: i64) -> Vec<SealedAudit> {
    let mut out: Vec<SealedAudit> = Vec::new();
    let mut prev = genesis(u(10), u(100));
    let created = facts(OrderState::Draft, 1, 0);
    let first = evidence(None)
        .committed_create(u(1), &created)
        .unwrap()
        .seal(1, prev)
        .unwrap();
    prev = first.entry_hash();
    out.push(first);
    for sequence in 2..=len {
        let before = facts(OrderState::Draft, 1, sequence - 1);
        let next = evidence(None)
            .committed_transition(
                u(u128::try_from(sequence).unwrap()),
                AuditTrigger::Public(Trigger::DraftMutate),
                &before,
                &before,
                None,
            )
            .unwrap()
            .seal(sequence, prev)
            .unwrap();
        prev = next.entry_hash();
        out.push(next);
    }
    out
}
fn verify(rows: &[(AuditRow, Vec<u8>)], counter: i64) -> Result<Option<ChainHead>, VerifyError> {
    let mut verifier = ChainVerifier::new(u(10), u(100));
    for (row, hash) in rows {
        verifier.push(row, hash)?;
    }
    verifier.finish(counter)
}
fn stored(chain: &[SealedAudit]) -> Vec<(AuditRow, Vec<u8>)> {
    chain
        .iter()
        .map(|s| (s.row().clone(), s.entry_hash().to_vec()))
        .collect()
}

#[test]
fn verifier_checks_genesis_continuity_binding_predecessors_digests_and_counter() {
    let rows = stored(&chain(4));
    assert_eq!(verify(&rows, 4).unwrap().unwrap().sequence, 4);
    assert_eq!(verify(&[], 0).unwrap(), None);
    // Shortened/empty trail against an intact counter.
    assert_eq!(
        verify(&rows[..3], 4),
        Err(VerifyError::Counter {
            head: 3,
            counter: 4
        })
    );
    assert_eq!(
        verify(&[], 1),
        Err(VerifyError::Counter {
            head: 0,
            counter: 1
        })
    );
    // Middle removal, reordering and duplicates break continuity.
    let middle = [rows[0].clone(), rows[2].clone()];
    assert!(matches!(
        verify(&middle, 4),
        Err(VerifyError::Sequence {
            expected: 2,
            found: Some(3)
        })
    ));
    let swapped = [rows[1].clone(), rows[0].clone()];
    assert!(matches!(
        verify(&swapped, 2),
        Err(VerifyError::Sequence { .. })
    ));
    // Changed content with its old stored digest.
    let mut changed = rows.clone();
    changed[2].0.reason = "draft-mutate ".into();
    assert!(verify(&changed, 4).is_err());
    let mut changed = rows.clone();
    changed[1].0.actor = Uuid::from_u128(31).to_string();
    assert!(matches!(
        verify(&changed, 4),
        Err(VerifyError::DigestMismatch { .. })
    ));
    // Rewritten digest without fixing the successor's predecessor.
    let mut rewritten = rows.clone();
    rewritten[1].0.correlation_id = None;
    rewritten[1].1 = entry_hash(&rewritten[1].0).unwrap().to_vec();
    assert!(matches!(
        verify(&rewritten, 4),
        Err(VerifyError::Predecessor { .. })
    ));
    // Wrong namespace (today's resource tenant is not the namespace) and another order.
    let mut verifier = ChainVerifier::new(u(11), u(100));
    assert!(matches!(
        verifier.push(&rows[0].0, &rows[0].1),
        Err(VerifyError::Binding { .. })
    ));
    let mut verifier = ChainVerifier::new(u(10), u(101));
    assert!(matches!(
        verifier.push(&rows[0].0, &rows[0].1),
        Err(VerifyError::Binding { .. })
    ));
    // A refusal is never part of the committed chain.
    let refusal = evidence(None)
        .resolved_refusal(
            u(50),
            AuditTrigger::Public(Trigger::Submit),
            &facts(OrderState::Draft, 1, 4),
            Reason::NotAdmissible,
        )
        .unwrap();
    let mut verifier = ChainVerifier::new(u(10), u(100));
    assert!(matches!(
        verifier.push(refusal.row(), &refusal.entry_hash()),
        Err(VerifyError::NotCommitted { .. })
    ));
}

#[test]
fn frozen_cross_version_links_verify_without_identity_resolution() {
    // v1 create -> v2 cancel, and v2 create -> v3 administrative edit: Python-authored digests.
    for (first, second) in [
        ("v1-create", "v2-after-v1-caller-reason"),
        ("v2-create", "v3-later-resource-change"),
    ] {
        let a = named(first);
        let b = named(second);
        let rows = [
            (row_of(&a["input"]), unhex(a["sha256"].as_str().unwrap())),
            (row_of(&b["input"]), unhex(b["sha256"].as_str().unwrap())),
        ];
        let tenant = rows[0].0.audit_tenant_id.unwrap();
        let order = rows[0].0.order_id.unwrap();
        let mut verifier = ChainVerifier::new(tenant, order);
        for (row, hash) in &rows {
            verifier.push(row, hash).unwrap();
        }
        assert_eq!(
            verifier.finish(2).unwrap().unwrap().entry_hash.to_vec(),
            rows[1].1
        );
    }
}

#[test]
fn checkpoint_encoding_refuses_unsorted_duplicate_miscounted_and_unknown_inputs() {
    let (header, members) = checkpoint_of(&named("checkpoint-2-members")["input"]);
    assert!(checkpoint_preimage(&header, &members).is_ok());
    let mut dup = members.clone();
    dup[1] = dup[0].clone();
    let reversed: Vec<_> = members.iter().rev().cloned().collect();
    let mut short = members.clone();
    short[0].entry_hash.pop();
    let mut zero = members.clone();
    zero[0].audit_sequence = 0;
    for bad in [dup, reversed, short, zero, members[..1].to_vec()] {
        assert!(checkpoint_preimage(&header, &bad).is_err());
    }
    let unknown = CheckpointHeader {
        format_version: 2,
        ..header
    };
    assert_eq!(
        checkpoint_preimage(&unknown, &members),
        Err(AuditError::UnsupportedVersion(2))
    );
    let (empty, none) = checkpoint_of(&named("checkpoint-0-members")["input"]);
    assert_eq!(
        hex(&checkpoint_hash(&empty, &none).unwrap()),
        named("checkpoint-0-members")["sha256"]
    );
}

/// S2-11: the streaming digest reproduces the frozen vectors byte for byte and refuses the
/// same malformed inputs as the slice encoder, in bounded memory.
#[test]
fn streaming_checkpoint_digest_matches_the_frozen_vectors_and_refuses_bad_streams() {
    for name in ["checkpoint-2-members", "checkpoint-0-members"] {
        let (header, members) = checkpoint_of(&named(name)["input"]);
        let mut streaming = CheckpointDigest::new(&header).unwrap();
        for member in &members {
            streaming.push(member).unwrap();
        }
        assert_eq!(streaming.pushed(), header.member_count);
        assert_eq!(hex(&streaming.finish().unwrap()), named(name)["sha256"]);
    }
    let (header, members) = checkpoint_of(&named("checkpoint-2-members")["input"]);
    // Unsorted and duplicate members are refused at the push that breaks the order.
    let mut reversed = CheckpointDigest::new(&header).unwrap();
    reversed.push(&members[1]).unwrap();
    assert!(reversed.push(&members[0]).is_err());
    let mut duplicate = CheckpointDigest::new(&header).unwrap();
    duplicate.push(&members[0]).unwrap();
    assert!(duplicate.push(&members[0]).is_err());
    // Fewer or more members than the header declares never produce a digest.
    let mut short = CheckpointDigest::new(&header).unwrap();
    short.push(&members[0]).unwrap();
    assert!(short.finish().is_err());
    let mut long = CheckpointDigest::new(&header).unwrap();
    long.push(&members[0]).unwrap();
    long.push(&members[1]).unwrap();
    let mut extra = members[1].clone();
    extra.order_id = Uuid::from_u128(u128::MAX);
    assert!(long.push(&extra).is_err());
    // Malformed members and unsupported headers are refused.
    let mut bad_hash = members[0].clone();
    bad_hash.entry_hash.pop();
    assert!(
        CheckpointDigest::new(&header)
            .unwrap()
            .push(&bad_hash)
            .is_err()
    );
    let unknown = CheckpointHeader {
        format_version: 2,
        ..header.clone()
    };
    assert!(CheckpointDigest::new(&unknown).is_err());
    let negative = CheckpointHeader {
        member_count: -1,
        ..header
    };
    assert!(CheckpointDigest::new(&negative).is_err());
}

/// D-204: the keyed minimizer is deterministic per key, differs across keys and key IDs, keeps
/// whitespace/case changes distinct, matches RFC 4231 HMAC-SHA256 and never exposes the key.
#[test]
fn keyed_admin_text_minimization_is_deterministic_per_key_and_never_exposes_the_key() {
    let k1 = test_key("k1", 7);
    let same = test_key("k1", 7);
    let other_key = test_key("k1", 8);
    let other_id = test_key("k2", 7);
    let a = minimized_text_digest(&k1, "Rack A");
    assert_eq!(
        a,
        minimized_text_digest(&same, "Rack A"),
        "deterministic per key"
    );
    assert_ne!(
        a,
        minimized_text_digest(&other_key, "Rack A"),
        "keys differ"
    );
    assert_ne!(
        a,
        minimized_text_digest(&other_id, "Rack A"),
        "rotation is visible"
    );
    assert!(minimized_text_digest(&other_id, "Rack A").starts_with("hmac-sha256:v1:k2:"));
    for (prior, new) in [
        ("Rack A", "rack a"),
        ("note", "note "),
        ("a\tb", "a b"),
        ("", " "),
    ] {
        assert_ne!(
            minimized_text_digest(&k1, prior),
            minimized_text_digest(&k1, new),
            "{prior:?} -> {new:?}"
        );
    }
    // RFC 4231 test case 2 shape check with a 32-byte key: the tag is HMAC-SHA256 itself.
    let rfc = AdminTextKey::new("rfc", &[0x0b; 32]).unwrap();
    let expected = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, &[0x0b; 32]), b"Hi There");
    assert_eq!(
        minimized_text_digest(&rfc, "Hi There"),
        format!("hmac-sha256:v1:rfc:{}", hex(expected.as_ref()))
    );
    // NULL stays NULL on both sides; a NULL <-> text change records only the tag.
    let order = facts(OrderState::Draft, 1, 1);
    let rows = evidence(None)
        .administrative_edits(
            &k1,
            &order,
            &[
                AdminChange {
                    field: AdminField::Order(AdminAttribute::DisplayLabel),
                    prior: None,
                    new: Some("Secret label".into()),
                },
                AdminChange {
                    field: AdminField::Order(AdminAttribute::InternalNotes),
                    prior: Some("Secret note".into()),
                    new: None,
                },
            ],
            Uuid::new_v4,
        )
        .unwrap();
    let (r0, r1) = (
        rows[0].clone().seal(2, [1; 32]).unwrap(),
        rows[1].clone().seal(3, [1; 32]).unwrap(),
    );
    assert_eq!(r0.row().prior_value, None);
    assert_eq!(
        r0.row().new_value,
        Some(minimized_text_digest(&k1, "Secret label"))
    );
    assert_eq!(r1.row().new_value, None);
    for row in [r0.row(), r1.row()] {
        let debug = format!("{row:?}");
        assert!(
            !debug.contains("Secret"),
            "no plaintext in the row or its Debug"
        );
    }
    // Key material never appears in Debug output; malformed configuration is refused.
    let debug = format!("{k1:?}");
    assert!(debug.contains("k1") && debug.contains("REDACTED") && !debug.contains("7, 7"));
    for (id, len) in [
        ("", 32),
        ("has:colon", 32),
        ("sp ace", 32),
        (&*"x".repeat(65), 32),
    ] {
        assert_eq!(
            AdminTextKey::new(id, &vec![1; len]).unwrap_err(),
            AdminTextKeyError::KeyId,
            "{id:?}"
        );
    }
    for len in [0, 31, 1025] {
        assert_eq!(
            AdminTextKey::new("k", &vec![1; len]).unwrap_err(),
            AdminTextKeyError::KeyLength
        );
    }
    assert!(AdminTextKey::new(&"x".repeat(64), &[1; 1024]).is_ok());
}
