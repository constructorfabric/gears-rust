#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use serde_json::json;

fn u(n: u128) -> Uuid {
    Uuid::from_u128(n)
}
fn vector(id: &str) -> Value {
    let corpus: Value =
        serde_json::from_str(include_str!("../../tests/fixtures/conformance-v1.json")).unwrap();
    corpus["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == id)
        .unwrap()
        .clone()
}
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, b| {
        write!(out, "{b:02x}").unwrap();
        out
    })
}
fn submit_input(contribution: &Value) -> FingerprintInput<'_> {
    FingerprintInput {
        operation: "submit",
        trigger: Trigger::Submit,
        target: FingerprintTarget::Order {
            order_id: u(100),
            expected_version: 4,
        },
        axes: FingerprintAxes {
            seller_tenant_id: u(20),
            resource_tenant_id: u(10),
            payer_tenant_id: u(30),
        },
        draft_revision: DraftRevisionInput::Expected(0),
        contribution,
    }
}

/// The runtime projection reproduces the independently authored (Python) frozen vectors,
/// byte-for-byte and by SHA-256, so the stored fingerprint is not self-certified.
#[test]
fn runtime_fingerprint_matches_frozen_independent_vectors() {
    let submit = vector("request-submit");
    let input = submit_input(&submit["input"]["contribution"]);
    assert_eq!(hex(&input.preimage().unwrap()), submit["preimage_hex"]);
    assert_eq!(
        input.fingerprint().unwrap().as_str(),
        format!("rf1:{}", submit["sha256"].as_str().unwrap())
    );
    let create = vector("request-create");
    let empty = json!({});
    let input = FingerprintInput {
        operation: "create",
        trigger: Trigger::Create,
        target: FingerprintTarget::Create,
        draft_revision: DraftRevisionInput::NotApplicable,
        ..submit_input(&empty)
    };
    assert_eq!(hex(&input.preimage().unwrap()), create["preimage_hex"]);
    assert_eq!(
        input.fingerprint().unwrap().as_str(),
        format!("rf1:{}", create["sha256"].as_str().unwrap())
    );
}

#[test]
fn every_semantic_input_changes_the_fingerprint_and_key_order_does_not() {
    let doc = vector("request-submit")["input"]["contribution"].clone();
    let base = submit_input(&doc).fingerprint().unwrap();
    let other_doc = json!({"acceptance": false, "lines": []});
    let variants = [
        FingerprintInput {
            operation: "amend",
            ..submit_input(&doc)
        },
        FingerprintInput {
            trigger: Trigger::Amendment,
            ..submit_input(&doc)
        },
        FingerprintInput {
            target: FingerprintTarget::Order {
                order_id: u(101),
                expected_version: 4,
            },
            ..submit_input(&doc)
        },
        FingerprintInput {
            target: FingerprintTarget::Order {
                order_id: u(100),
                expected_version: 5,
            },
            ..submit_input(&doc)
        },
        FingerprintInput {
            axes: FingerprintAxes {
                seller_tenant_id: u(21),
                ..submit_input(&doc).axes
            },
            ..submit_input(&doc)
        },
        FingerprintInput {
            axes: FingerprintAxes {
                resource_tenant_id: u(11),
                ..submit_input(&doc).axes
            },
            ..submit_input(&doc)
        },
        FingerprintInput {
            axes: FingerprintAxes {
                payer_tenant_id: u(31),
                ..submit_input(&doc).axes
            },
            ..submit_input(&doc)
        },
        FingerprintInput {
            draft_revision: DraftRevisionInput::Expected(1),
            ..submit_input(&doc)
        },
        FingerprintInput {
            draft_revision: DraftRevisionInput::NotApplicable,
            ..submit_input(&doc)
        },
        submit_input(&other_doc),
    ];
    for variant in &variants {
        assert_ne!(variant.fingerprint().unwrap(), base, "{variant:?}");
    }
    let reordered: Value =
        serde_json::from_str(r#"{"lines":[{"note":"caf\u00e9","quantity":"1.2300","line_id":"00000000-0000-0000-0000-000000000065"}],"acceptance":true}"#)
            .unwrap();
    assert_eq!(submit_input(&reordered).fingerprint().unwrap(), base);
    // Omitted and explicit-null document members stay distinct.
    let absent = json!({});
    let null = json!({"note": null});
    assert_ne!(
        submit_input(&absent).fingerprint().unwrap(),
        submit_input(&null).fingerprint().unwrap()
    );
}

#[test]
fn sentinels_targets_and_numbers_are_strict() {
    let doc = json!({});
    // Only registered idempotent writes enter the hash: reads, Preview and free text refuse.
    for operation in [
        "get",
        "list_versions",
        "preview",
        "submit ",
        "Submit",
        "draft-mutate",
        "",
    ] {
        assert_eq!(
            FingerprintInput {
                operation,
                ..submit_input(&doc)
            }
            .preimage(),
            Err(RegistryRuleError::UnknownOperation),
            "{operation:?}"
        );
    }
    // The internal worker operations are registered writes too (07 §3.1); their REST-like
    // spellings are not.
    for operation in [EXPIRE_OPERATION, AUTO_VOID_OPERATION] {
        assert!(
            FingerprintInput {
                operation,
                ..submit_input(&doc)
            }
            .preimage()
            .is_ok(),
            "{operation}"
        );
    }
    assert_eq!(
        FingerprintInput {
            operation: "auto-void",
            ..submit_input(&doc)
        }
        .preimage(),
        Err(RegistryRuleError::UnknownOperation)
    );
    assert!(
        FingerprintInput {
            operation: REPLACE_FULFILLMENT_GRANT_OPERATION,
            ..submit_input(&doc)
        }
        .preimage()
        .is_ok()
    );
    // Create carries only the sentinel; non-create must carry target and positive version.
    assert_eq!(
        FingerprintInput {
            trigger: Trigger::Create,
            ..submit_input(&doc)
        }
        .preimage(),
        Err(RegistryRuleError::InvalidTarget)
    );
    assert_eq!(
        FingerprintInput {
            target: FingerprintTarget::Create,
            ..submit_input(&doc)
        }
        .preimage(),
        Err(RegistryRuleError::InvalidTarget)
    );
    assert_eq!(
        FingerprintInput {
            target: FingerprintTarget::Order {
                order_id: u(1),
                expected_version: 0
            },
            ..submit_input(&doc)
        }
        .preimage(),
        Err(RegistryRuleError::InvalidTarget)
    );
    assert_eq!(
        FingerprintInput {
            draft_revision: DraftRevisionInput::Expected(-1),
            ..submit_input(&doc)
        }
        .preimage(),
        Err(RegistryRuleError::InvalidTarget)
    );
    let number = json!({"quantity": 1});
    assert_eq!(
        submit_input(&number).preimage(),
        Err(RegistryRuleError::NonCanonical)
    );
}

#[test]
fn retention_class_is_exactly_the_workflow_trigger_class() {
    for trigger in Trigger::ALL {
        let class = RegistryOperation::Trigger(*trigger).retention();
        assert_eq!(
            class == RetentionClass::Workflow,
            WORKFLOW_CLASS.contains(trigger),
            "{trigger:?}"
        );
    }
    assert_eq!(
        RegistryOperation::Trigger(Trigger::ForceFailUnreconciled).retention(),
        RetentionClass::Ordinary
    );
    // D-203: the internal rebuild is retried by Workflow under one key and keeps 30 days.
    assert_eq!(
        RegistryOperation::ReplaceFulfillmentGrant.retention(),
        RetentionClass::Workflow
    );
    assert_eq!(RetentionClass::Ordinary.window(), Duration::hours(24));
    assert!(RetentionClass::Workflow.window() >= Duration::days(30));
    assert_eq!(
        RegistryOperation::ReplaceFulfillmentGrant.token(),
        "replace-fulfillment-grant"
    );
    assert_eq!(
        RegistryOperation::Trigger(Trigger::Amendment).token(),
        "amendment"
    );
}

#[test]
fn lease_must_be_positive_and_finite() {
    assert_eq!(
        LeaseDuration::from_seconds(0),
        Err(RegistryRuleError::InvalidLease)
    );
    assert_eq!(
        LeaseDuration::from_seconds(86_401),
        Err(RegistryRuleError::InvalidLease)
    );
    assert_eq!(
        LeaseDuration::from_seconds(30).unwrap().duration(),
        Duration::seconds(30)
    );
}

#[test]
fn principal_scope_is_the_stable_tenant_subject_pair_only() {
    let ctx = |tenant: Uuid, subject: Uuid, token: &str| {
        SecurityContext::builder()
            .subject_id(subject)
            .subject_tenant_id(tenant)
            .subject_type("user")
            .bearer_token(token.to_owned())
            .build()
            .unwrap()
    };
    let a = PrincipalScope::from_context(&ctx(u(10), u(40), "token-1")).unwrap();
    // A retry with a fresh token/session is the same principal.
    let b = PrincipalScope::from_context(&ctx(u(10), u(40), "token-2")).unwrap();
    assert_eq!(a, b);
    assert_eq!(a.as_str(), format!("{}/{}", u(10), u(40)));
    assert_ne!(
        a,
        PrincipalScope::from_context(&ctx(u(10), u(41), "token-1")).unwrap()
    );
    assert_ne!(
        a,
        PrincipalScope::from_context(&ctx(u(11), u(40), "token-1")).unwrap()
    );
    assert!(!format!("{a:?}").contains(&u(40).to_string()));
    // Injective, unambiguous text: two fixed-width lowercase hyphenated UUIDs whose alphabet
    // excludes the separator, so the pair is recovered exactly from any scope string.
    for (tenant, subject) in [
        (u(10), u(40)),
        (Uuid::from_u128(u128::MAX), Uuid::from_u128(u128::MAX - 1)),
        (
            Uuid::from_u128(0xABCD_EF00 << 64),
            Uuid::from_u128(0xFEDC_BA98),
        ),
    ] {
        let scope = PrincipalScope::from_context(&ctx(tenant, subject, "t")).unwrap();
        let text = scope.as_str();
        assert_eq!(text.len(), 73);
        assert_eq!(text, text.to_ascii_lowercase());
        let parts: Vec<_> = text.split('/').collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(
            (
                Uuid::parse_str(parts[0]).unwrap(),
                Uuid::parse_str(parts[1]).unwrap()
            ),
            (tenant, subject)
        );
        assert_eq!(parts[0], tenant.hyphenated().to_string());
    }
    assert_eq!(
        PrincipalScope::from_context(&SecurityContext::anonymous()),
        Err(RegistryRuleError::UnstablePrincipal)
    );
    assert_eq!(
        PrincipalScope::from_context(&ctx(Uuid::nil(), u(40), "t")),
        Err(RegistryRuleError::UnstablePrincipal)
    );
}

fn at(seconds: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_790_000_000 + seconds).unwrap()
}
fn view(settled: bool, lease: Option<i64>, expires: i64) -> RecordView<'static> {
    RecordView {
        fingerprint: "rf1:a",
        settled,
        lease_expires_at: lease.map(at),
        expires_at: at(expires),
        unresolved_execution: false,
        owner: None,
    }
}

/// Every registry state × same/different fingerprint × lease/retention boundary.
#[test]
fn gate_classifies_expiry_then_fingerprint_then_settlement_and_lease() {
    let same = Fingerprint("rf1:a".into());
    let other = Fingerprint("rf1:b".into());
    let now = at(0);
    let c = |v, f: &Fingerprint| classify(v, f, now, None).unwrap();
    // Settled, live retention.
    assert_eq!(c(view(true, None, 10), &same), GateDecision::Replay);
    assert_eq!(c(view(true, None, 10), &other), GateDecision::Mismatch);
    // Settled at/after the exact retention boundary: logical expiry, replaceable either way.
    assert_eq!(c(view(true, None, 0), &same), GateDecision::ReplaceExpired);
    assert_eq!(
        c(view(true, None, -1), &other),
        GateDecision::ReplaceExpired
    );
    // In flight: live lease, exact lease boundary (expired), expired lease.
    assert_eq!(
        c(view(false, Some(1), 10), &same),
        GateDecision::StillProcessing
    );
    assert_eq!(c(view(false, Some(1), 10), &other), GateDecision::Mismatch);
    assert_eq!(c(view(false, Some(0), 10), &same), GateDecision::Reclaim);
    assert_eq!(c(view(false, Some(-5), 10), &other), GateDecision::Mismatch);
    // In flight past retention but with a live lease is preserved (both deadlines needed).
    assert_eq!(
        c(view(false, Some(1), -1), &same),
        GateDecision::StillProcessing
    );
    assert_eq!(c(view(false, Some(1), -1), &other), GateDecision::Mismatch);
    assert_eq!(
        c(view(false, Some(-1), -1), &other),
        GateDecision::ReplaceExpired
    );
    // An unresolved durable execution survives response-key expiry.
    let mut durable = view(false, Some(-1), -1);
    durable.unresolved_execution = true;
    assert_eq!(c(durable, &same), GateDecision::Reclaim);
    assert_eq!(c(durable, &other), GateDecision::Mismatch);
    // The presenting executor recognizes its own exact owner/fence, live or expired lease.
    let owner = ExecutionOwner {
        execution_id: u(1),
        owner_token: u(2),
        fencing_generation: 3,
    };
    durable.owner = Some(owner);
    durable.lease_expires_at = Some(at(5));
    assert_eq!(
        classify(durable, &same, now, Some(&owner)).unwrap(),
        GateDecision::CurrentOwner
    );
    let stale = ExecutionOwner {
        fencing_generation: 2,
        ..owner
    };
    assert_eq!(
        classify(durable, &same, now, Some(&stale)).unwrap(),
        GateDecision::StillProcessing
    );
    assert_eq!(
        classify(durable, &other, now, Some(&owner)).unwrap(),
        GateDecision::Mismatch
    );
    assert_eq!(
        classify(view(false, None, 10), &same, now, None),
        Err(RegistryRuleError::InvalidRecord)
    );
}

#[test]
fn fences_increase_checked_and_never_wrap() {
    assert_eq!(next_fence(0), Ok(1));
    assert_eq!(next_fence(i64::MAX), Err(RegistryRuleError::FenceExhausted));
    assert_eq!(next_fence(-1), Err(RegistryRuleError::InvalidRecord));
}

#[test]
fn stored_response_is_versioned_closed_and_credential_free() {
    let mut headers = BTreeMap::new();
    headers.insert("ETag".to_owned(), "\"4\"".to_owned());
    let response = StoredResponse::new(201, json!({"orderId": u(1)}), headers, None).unwrap();
    let encoded = response.encode().unwrap();
    assert_eq!(encoded["formatVersion"], 1);
    assert_eq!(StoredResponse::decode(&encoded).unwrap(), response);
    for bad in [
        json!({"status": 201, "body": {}}),
        json!({"formatVersion": 2, "status": 201, "body": {}}),
        json!({"formatVersion": "1", "status": 201, "body": {}}),
    ] {
        assert_eq!(
            StoredResponse::decode(&bad),
            Err(RegistryRuleError::UnsupportedResponseFormat)
        );
    }
    let mut extra = encoded;
    extra["unknown"] = json!(true);
    assert_eq!(
        StoredResponse::decode(&extra),
        Err(RegistryRuleError::InvalidResponse)
    );
    // Closed semantic set: credentials, proof references and arbitrary transport headers
    // (including ones a denylist would miss) are refused.
    for header in [
        "Authorization",
        "set-cookie",
        "X-Delegation-Proof-Ref",
        "X-Api-Key",
        "WWW-Authenticate",
        "traceparent",
        "X-Request-Id",
    ] {
        let mut h = BTreeMap::new();
        h.insert(header.to_owned(), "x".to_owned());
        assert_eq!(
            StoredResponse::new(200, json!({}), h, None),
            Err(RegistryRuleError::InvalidResponse),
            "{header}"
        );
    }
    let mut semantic = BTreeMap::new();
    semantic.insert("ETag".to_owned(), "\"4\"".to_owned());
    semantic.insert("Location".to_owned(), format!("/orders/{}", u(1)));
    semantic.insert("content-type".to_owned(), "application/json".to_owned());
    assert!(StoredResponse::new(201, json!({}), semantic, None).is_ok());
    for (a, b) in [("ETag", "etag"), ("Location", "LOCATION")] {
        let mut h = BTreeMap::new();
        h.insert(a.to_owned(), "\"1\"".to_owned());
        h.insert(b.to_owned(), "\"2\"".to_owned());
        assert_eq!(
            StoredResponse::new(200, json!({}), h, None),
            Err(RegistryRuleError::InvalidResponse),
            "case-duplicated {a}"
        );
    }
    for value in ["", "\"4\"\r\nSet-Cookie: x", "caf\u{e9}"] {
        let mut h = BTreeMap::new();
        h.insert("ETag".to_owned(), value.to_owned());
        assert_eq!(
            StoredResponse::new(200, json!({}), h, None),
            Err(RegistryRuleError::InvalidResponse),
            "{value:?}"
        );
    }
    // A stored snapshot carrying a non-semantic header is refused on decode too.
    let smuggled =
        json!({"formatVersion": 1, "status": 200, "body": {}, "headers": {"cookie": "a=b"}});
    assert_eq!(
        StoredResponse::decode(&smuggled),
        Err(RegistryRuleError::InvalidResponse)
    );
    assert_eq!(
        StoredResponse::new(99, json!({}), BTreeMap::new(), None),
        Err(RegistryRuleError::InvalidResponse)
    );
}
