//! The S1-06/S5-01 frozen independent vectors (Python-authored preimages and SHA-256 digests)
//! against the production encoders: the audit row preimage and entry hash, the chain and
//! roll-up genesis, the checkpoint roll-up (slice and streaming forms) and the request
//! fingerprint. The vectors are not self-certified: nothing here re-implements an encoding.
//!
//! The fixture is string-typed; `row_of`, `checkpoint_of` and `request_of` convert it to the
//! stored typed values, as `domain/audit_tests.rs` does. A value the stored type cannot hold
//! (an out-of-range integer, a missing required member) is refused by that conversion; every
//! other refusal below comes from the production encoder or shape check.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a corrupt frozen fixture must fail loudly; `allow`, not `expect`, because not every \
              test in the file unwraps or expects"
)]
use bss_orders::domain::audit::{
    AuditRow, CheckpointDigest, CheckpointHeader, CheckpointMember, ForceRequestObservation,
    check_shape, checkpoint_genesis, checkpoint_hash, checkpoint_preimage, entry_hash, genesis,
    genesis_preimage, preimage,
};
use bss_orders::domain::idempotency::{
    DraftRevisionInput, FingerprintAxes, FingerprintInput, FingerprintTarget,
};
use bss_orders_sdk::catalog::Trigger;
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

fn vectors() -> Vec<Value> {
    serde_json::from_str::<Value>(include_str!("fixtures/conformance-v1.json")).unwrap()["vectors"]
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
/// SHA-256 of production bytes (the FIPS provider production hashes with); not an encoder.
fn sha(bytes: &[u8]) -> String {
    hex(toolkit_security::crypto::hash::Sha256::digest(bytes).as_ref())
}
fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// A nullable fixture member: `None` for JSON null, `Some(Err)` for a value the type refuses.
fn opt<T>(v: &Value, parse: impl FnOnce(&str) -> Option<T>) -> Result<Option<T>, ()> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => parse(s).map(Some).ok_or(()),
        _ => Err(()),
    }
}
fn req<T>(v: &Value, parse: impl FnOnce(&str) -> Option<T>) -> Result<T, ()> {
    opt(v, parse)?.ok_or(())
}
fn uuid(s: &str) -> Option<Uuid> {
    Uuid::parse_str(s).ok()
}
fn owned(s: &str) -> Option<String> {
    s.parse().ok()
}
fn int<T: std::str::FromStr>(s: &str) -> Option<T> {
    s.parse().ok()
}
fn instant(s: &str) -> Option<OffsetDateTime> {
    let micros: i64 = s.parse().ok()?;
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(micros) * 1_000).ok()
}

/// The fixture's string-typed row as the stored typed row; `Err` where a column type cannot
/// hold the value.
fn row_of(input: &Value) -> Result<AuditRow, ()> {
    Ok(AuditRow {
        hash_version: input["hash_version"]
            .as_i64()
            .and_then(|v| i16::try_from(v).ok())
            .ok_or(())?,
        audit_id: req(&input["audit_id"], uuid)?,
        audit_tenant_id: opt(&input["audit_tenant_id"], uuid)?,
        subject_tenant_id: req(&input["subject_tenant_id"], uuid)?,
        resource_tenant_id: opt(&input["resource_tenant_id"], uuid)?,
        order_id: opt(&input["order_id"], uuid)?,
        requested_order_ref: opt(&input["requested_order_ref"], uuid)?,
        sequence: opt(&input["sequence"], int)?,
        from_state: opt(&input["from_state"], owned)?,
        to_state: opt(&input["to_state"], owned)?,
        trigger: req(&input["trigger"], owned)?,
        outcome: req(&input["outcome"], owned)?,
        actor: req(&input["actor"], owned)?,
        actor_class: req(&input["actor_class"], owned)?,
        delegation_proof_ref: opt(&input["delegation_proof_ref"], owned)?,
        reason: req(&input["reason"], owned)?,
        changed_field: opt(&input["changed_field"], owned)?,
        prior_value: opt(&input["prior_value"], owned)?,
        new_value: opt(&input["new_value"], owned)?,
        idempotency_key: req(&input["idempotency_key"], owned)?,
        correlation_id: opt(&input["correlation_id"], uuid)?,
        version: opt(&input["version"], int)?,
        created_at: req(&input["created_at"], instant)?,
        prev_hash: opt(&input["prev_hash"], unhex)?,
        caller_reason: opt(&input["caller_reason"], owned)?,
        force_request_observation: match input.get("force_request_observation") {
            None | Some(Value::Null) => None,
            Some(o) => Some(observation_of(o)?),
        },
    })
}
/// The fixture's string-typed observation through the production stored-JSON decode, which
/// refuses unknown, missing and mistyped members (the persisted form carries JSON numbers).
fn observation_of(o: &Value) -> Result<ForceRequestObservation, ()> {
    let mut stored = o.as_object().ok_or(())?.clone();
    for key in ["audit_sequence", "version"] {
        if let Some(value) = stored.get_mut(key) {
            let n: i64 = value.as_str().and_then(int).ok_or(())?;
            *value = json!(n);
        }
    }
    ForceRequestObservation::from_json(&Value::Object(stored)).map_err(drop)
}
fn checkpoint_of(input: &Value) -> Result<(CheckpointHeader, Vec<CheckpointMember>), ()> {
    let header = CheckpointHeader {
        format_version: input["format_version"]
            .as_i64()
            .and_then(|v| i16::try_from(v).ok())
            .ok_or(())?,
        audit_tenant_id: req(&input["audit_tenant_id"], uuid)?,
        checkpoint_sequence: req(&input["checkpoint_sequence"], int)?,
        captured_at: req(&input["captured_at"], instant)?,
        member_count: req(&input["member_count"], int)?,
        prev_checkpoint_hash: req(&input["prev_checkpoint_hash"], unhex)?,
    };
    let members = input["members"]
        .as_array()
        .ok_or(())?
        .iter()
        .map(|m| {
            Ok(CheckpointMember {
                order_id: req(&m["order_id"], uuid)?,
                audit_sequence: req(&m["audit_sequence"], int)?,
                entry_hash: req(&m["entry_hash"], unhex)?,
            })
        })
        .collect::<Result<_, ()>>()?;
    Ok((header, members))
}
/// The streaming roll-up the checkpoint worker uses, over the same header and members.
fn streamed(header: &CheckpointHeader, members: &[CheckpointMember]) -> bool {
    let Ok(mut digest) = CheckpointDigest::new(header) else {
        return false;
    };
    members.iter().all(|m| digest.push(m).is_ok()) && digest.finish().is_ok()
}
fn trigger_of(token: &str) -> Trigger {
    *Trigger::ALL.iter().find(|t| t.token() == token).unwrap()
}
/// The fixture's request as the production fingerprint input.
fn request_of(input: &Value) -> FingerprintInput<'_> {
    let target = match opt(&input["order_id"], uuid).unwrap() {
        None => FingerprintTarget::Create,
        Some(order_id) => FingerprintTarget::Order {
            order_id,
            expected_version: req(&input["expected_version"], int).unwrap(),
        },
    };
    FingerprintInput {
        operation: input["operation"].as_str().unwrap(),
        trigger: trigger_of(input["trigger"].as_str().unwrap()),
        target,
        axes: FingerprintAxes {
            seller_tenant_id: req(&input["seller_tenant_id"], uuid).unwrap(),
            resource_tenant_id: req(&input["resource_tenant_id"], uuid).unwrap(),
            payer_tenant_id: req(&input["payer_tenant_id"], uuid).unwrap(),
        },
        draft_revision: opt(&input["expected_draft_revision"], int).unwrap().map_or(
            DraftRevisionInput::NotApplicable,
            DraftRevisionInput::Expected,
        ),
        contribution: &input["contribution"],
    }
}

/// `F(x) || F(y)` for two nullable texts, as the production audit preimage frames them: the
/// row's adjacent nullable `prior_value` and `new_value` columns are set to the pair and the
/// frames are cut out of the production bytes. The cut points come from production too: the
/// first byte where a NULL and a present `prior_value` differ, and the last byte where a NULL
/// and a present `new_value` differ.
fn production_frames(x: Option<&str>, y: Option<&str>) -> Vec<u8> {
    let base = row_of(&named("v2-create")["input"]).unwrap();
    let with = |prior: Option<&str>, new: Option<&str>| {
        preimage(&AuditRow {
            prior_value: prior.map(str::to_owned),
            new_value: new.map(str::to_owned),
            ..base.clone()
        })
        .unwrap()
    };
    let nulls = with(None, None);
    let start = nulls
        .iter()
        .zip(&with(Some("x"), None))
        .position(|(a, b)| a != b)
        .unwrap();
    let end = nulls
        .iter()
        .rev()
        .zip(with(None, Some("x")).iter().rev())
        .position(|(a, b)| a != b)
        .unwrap();
    let bytes = with(x, y);
    bytes[start..bytes.len() - end].to_vec()
}

#[test]
fn production_encoders_reproduce_every_frozen_independent_vector() {
    let mut seen = std::collections::BTreeMap::<String, usize>::new();
    let mut shape_invalid = Vec::new();
    for vector in vectors() {
        let kind = vector["kind"].as_str().unwrap();
        *seen.entry(kind.to_owned()).or_default() += 1;
        let (id, input) = (&vector["id"], &vector["input"]);
        let (bytes, digest) = match kind {
            "audit" => {
                let row = row_of(input).unwrap();
                let bytes = preimage(&row).unwrap();
                let digest = sha(&bytes);
                // The stored entry hash of a shape-valid row is the same digest.
                match entry_hash(&row) {
                    Ok(stored) => assert_eq!(hex(&stored), digest, "{id}"),
                    Err(_) => shape_invalid.push(id.as_str().unwrap().to_owned()),
                }
                (bytes, digest)
            }
            "genesis" => {
                let (tenant, order) = (
                    req(&input["audit_tenant_id"], uuid).unwrap(),
                    req(&input["order_id"], uuid).unwrap(),
                );
                (
                    genesis_preimage(tenant, order),
                    hex(&genesis(tenant, order)),
                )
            }
            "checkpoint" => {
                let (header, members) = checkpoint_of(input).unwrap();
                assert!(streamed(&header, &members), "{id}");
                let mut stream = CheckpointDigest::new(&header).unwrap();
                for member in &members {
                    stream.push(member).unwrap();
                }
                let digest = hex(&checkpoint_hash(&header, &members).unwrap());
                assert_eq!(hex(&stream.finish().unwrap()), digest, "{id}");
                (checkpoint_preimage(&header, &members).unwrap(), digest)
            }
            "checkpoint_genesis" => {
                let digest = checkpoint_genesis(req(&input["audit_tenant_id"], uuid).unwrap());
                assert_eq!(hex(&digest), vector["sha256"], "{id}");
                continue;
            }
            "request" => {
                let request = request_of(input);
                let fingerprint = request.fingerprint().unwrap();
                let digest = fingerprint
                    .as_str()
                    .strip_prefix("rf1:")
                    .unwrap()
                    .to_owned();
                (request.preimage().unwrap(), digest)
            }
            "framing" => {
                let items: Vec<Option<&str>> = input
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(Value::as_str)
                    .collect();
                let bytes = items
                    .chunks(2)
                    .flat_map(|pair| production_frames(pair[0], pair[1]))
                    .collect::<Vec<_>>();
                // The framing digest is the Python oracle's; the bytes are what production frames.
                assert_eq!(hex(&bytes), vector["preimage_hex"], "{id}");
                continue;
            }
            other => panic!("unhandled vector kind {other}"),
        };
        assert_eq!(hex(&bytes), vector["preimage_hex"], "{id}");
        assert_eq!(digest, vector["sha256"], "{id}");
    }
    let expected: std::collections::BTreeMap<String, usize> = [
        ("audit", 20),
        ("checkpoint", 2),
        ("checkpoint_genesis", 1),
        ("framing", 5),
        ("genesis", 1),
        ("request", 3),
    ]
    .into_iter()
    .map(|(k, n)| (k.to_owned(), n))
    .collect();
    assert_eq!(seen, expected, "every vector kind is exercised");
    // The one framing-boundary row a writer never produces: an empty caller reason.
    assert_eq!(shape_invalid, ["v2-caller-empty"]);
}

#[test]
fn every_audit_field_changes_bytes_or_is_rejected() {
    let base = named("v2-after-v1-caller-reason")["input"].clone();
    let original = preimage(&row_of(&base).unwrap()).unwrap();
    for (key, value) in base.as_object().unwrap() {
        let mut changed = base.clone();
        changed[key] = if value.is_null() {
            json!("changed")
        } else {
            Value::Null
        };
        if let Ok(bytes) = row_of(&changed).and_then(|row| preimage(&row).map_err(drop)) {
            assert_ne!(bytes, original, "uncovered field {key}");
        }
    }
    // Refused by the production encoder.
    for (key, value) in [
        ("hash_version", json!(4)),
        ("sequence", json!("0")),
        ("version", json!("-1")),
        ("prev_hash", json!("abcd")),
    ] {
        let mut bad = base.clone();
        bad[key] = value;
        assert!(preimage(&row_of(&bad).unwrap()).is_err(), "accepted {key}");
    }
    // Outside the stored column types.
    for (key, value) in [
        ("sequence", json!("9223372036854775808")),
        ("version", json!("2147483648")),
        ("created_at", json!("1.25")),
    ] {
        let mut bad = base.clone();
        bad[key] = value;
        assert!(row_of(&bad).is_err(), "accepted {key}");
    }
    let mut v1 = base;
    v1["hash_version"] = json!(1);
    assert!(
        preimage(&row_of(&v1).unwrap()).is_err(),
        "v1 cannot silently omit caller evidence"
    );
}

#[test]
fn null_empty_boundaries_and_cross_version_links_are_distinct() {
    for (a, b) in [
        ("null-empty", "empty-null"),
        ("adjacent-ab-c", "adjacent-a-bc"),
        ("v2-caller-null", "v2-caller-empty"),
    ] {
        assert_ne!(named(a)["preimage_hex"], named(b)["preimage_hex"]);
        assert_ne!(named(a)["sha256"], named(b)["sha256"]);
    }
    // The production encoder keeps a NULL caller reason and an empty one apart.
    let null = row_of(&named("v2-caller-null")["input"]).unwrap();
    let empty = row_of(&named("v2-caller-empty")["input"]).unwrap();
    assert_ne!(preimage(&null).unwrap(), preimage(&empty).unwrap());
    assert_eq!(
        named("v2-after-v1-caller-reason")["input"]["prev_hash"],
        named("v1-create")["sha256"]
    );
    for version in [1, 2] {
        assert_eq!(
            named(&format!("v{version}-later-resource-change"))["input"]["prev_hash"],
            named(&format!("v{version}-create"))["sha256"]
        );
        assert_eq!(
            named(&format!("v{version}-create"))["input"]["prev_hash"],
            named("order-genesis")["sha256"]
        );
    }
}

#[test]
fn checkpoint_duplicate_unsorted_miscounted_and_unknown_inputs_refuse() {
    let base = named("checkpoint-2-members")["input"].clone();
    for mode in 0..4 {
        let mut bad = base.clone();
        match mode {
            0 => bad["members"][1] = bad["members"][0].clone(),
            1 => bad["members"].as_array_mut().unwrap().reverse(),
            2 => bad["member_count"] = json!("3"),
            _ => bad["format_version"] = json!(2),
        }
        let (header, members) = checkpoint_of(&bad).unwrap();
        assert!(
            checkpoint_preimage(&header, &members).is_err(),
            "case {mode}"
        );
        assert!(!streamed(&header, &members), "streamed case {mode}");
    }
    let mut missing = base;
    missing["members"][0]
        .as_object_mut()
        .unwrap()
        .remove("entry_hash");
    assert!(checkpoint_of(&missing).is_err());
}

/// Object keys are ordered by UTF-16 code units (RFC 8785), not by UTF-8 bytes: U+1F600 (a
/// surrogate pair, 0xD83D...) sorts before U+FF61 under UTF-16 and after it under UTF-8. The
/// frozen `request-utf16-key-order` vector pins the order; the same members authored in UTF-8
/// order produce the same fingerprint, and a byte-ordered preimage would not match the fixture.
#[test]
fn fingerprint_keys_sort_by_utf16_code_units() {
    let vector = named("request-utf16-key-order");
    let input = &vector["input"];
    let preimage = request_of(input).preimage().unwrap();
    assert_eq!(hex(&preimage), vector["preimage_hex"]);
    let text = String::from_utf8(preimage).unwrap();
    let emoji = text.find('\u{1F600}').unwrap();
    let halfwidth = text.find('\u{FF61}').unwrap();
    assert!(emoji < halfwidth, "UTF-16 order puts U+1F600 first");
    assert!(
        "\u{FF61}".as_bytes() < "\u{1F600}".as_bytes(),
        "UTF-8 order differs"
    );
    let mut reordered = input.clone();
    reordered["contribution"] =
        serde_json::from_str(r#"{"\uff61":"halfwidth","\ud83d\ude00":"emoji","acceptance":true}"#)
            .unwrap();
    assert_eq!(
        request_of(&reordered).fingerprint().unwrap(),
        request_of(input).fingerprint().unwrap()
    );
}

/// One named change to a fingerprint input.
type Change = (&'static str, Box<dyn Fn(&mut FingerprintInput<'_>)>);

#[test]
fn request_fingerprint_fixture_binds_each_semantic_input() {
    let base = named("request-submit")["input"].clone();
    let original = request_of(&base).fingerprint().unwrap();
    let other = Uuid::from_u128(999);
    let contribution = json!({"acceptance": false});
    let changes: [Change; 8] = [
        ("operation", Box::new(|r| r.operation = "expire")),
        ("trigger", Box::new(|r| r.trigger = Trigger::DraftMutate)),
        (
            "order_id",
            Box::new(move |r| {
                r.target = FingerprintTarget::Order {
                    order_id: other,
                    expected_version: 4,
                };
            }),
        ),
        (
            "expected_version",
            Box::new(|r| {
                if let FingerprintTarget::Order {
                    expected_version, ..
                } = &mut r.target
                {
                    *expected_version = 5;
                }
            }),
        ),
        (
            "seller_tenant_id",
            Box::new(move |r| r.axes.seller_tenant_id = other),
        ),
        (
            "resource_tenant_id",
            Box::new(move |r| r.axes.resource_tenant_id = other),
        ),
        (
            "payer_tenant_id",
            Box::new(move |r| r.axes.payer_tenant_id = other),
        ),
        (
            "expected_draft_revision",
            Box::new(|r| r.draft_revision = DraftRevisionInput::NotApplicable),
        ),
    ];
    for (key, change) in changes {
        let mut request = request_of(&base);
        change(&mut request);
        // A change is either a different fingerprint or refused (an unregistered pairing).
        if let Ok(changed) = request.fingerprint() {
            assert_ne!(changed, original, "{key}");
        }
    }
    let mut request = request_of(&base);
    request.contribution = &contribution;
    assert_ne!(request.fingerprint().unwrap(), original, "contribution");
    // Reordering object keys is irrelevant; array order and exact scalar strings remain semantic.
    let mut reordered = serde_json::Map::new();
    for (key, value) in base["contribution"].as_object().unwrap().iter().rev() {
        reordered.insert(key.clone(), value.clone());
    }
    let reordered = Value::Object(reordered);
    let mut request = request_of(&base);
    request.contribution = &reordered;
    assert_eq!(request.fingerprint().unwrap(), original);
    let mut numeric = base["contribution"].clone();
    numeric["lines"][0]["quantity"] = json!(1.23);
    let mut request = request_of(&base);
    request.contribution = &numeric;
    assert!(
        request.fingerprint().is_err(),
        "the profile requires exact numeric strings"
    );
    for excluded in [
        "correlation_id",
        "requested_at",
        "headers",
        "server_snapshot",
        "principal_scope",
        "idempotency_key",
    ] {
        assert!(
            base.get(excluded).is_none(),
            "not fingerprint input: {excluded}"
        );
    }
    let create = named("request-create");
    assert!(create["input"]["order_id"].is_null());
    assert!(create["input"]["expected_version"].is_null());
}

#[test]
fn force_request_observation_is_hashed_closed_and_never_a_chain_sequence() {
    let input = named("v3-force-request")["input"].clone();
    let row = row_of(&input).unwrap();
    let original = preimage(&row).unwrap();
    check_shape(&row).unwrap();
    assert!(input["sequence"].is_null() && input["prev_hash"].is_null());
    for field in ["audit_sequence", "state", "version"] {
        assert_ne!(
            named(&format!("v3-force-changed-{field}"))["preimage_hex"],
            named("v3-force-request")["preimage_hex"]
        );
        let mut bad = input.clone();
        bad["force_request_observation"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(row_of(&bad).is_err(), "missing {field}");
    }
    // Refused by the stored-JSON decode (closed object, typed members).
    for value in [
        json!({}),
        json!({"audit_sequence":"55","state":"in_fulfillment","version":"7","extra":true}),
        json!({"audit_sequence":"9223372036854775808","state":"in_fulfillment","version":"7"}),
    ] {
        let mut bad = input.clone();
        bad["force_request_observation"] = value;
        assert!(row_of(&bad).is_err());
    }
    // Refused by the production encoder or shape check.
    for value in [
        json!(null),
        json!({"audit_sequence":"0","state":"in_fulfillment","version":"7"}),
        json!({"audit_sequence":"55","state":"invented","version":"7"}),
        json!({"audit_sequence":"55","state":"in_fulfillment","version":"4"}),
    ] {
        let mut bad = input.clone();
        bad["force_request_observation"] = value.clone();
        let row = row_of(&bad).unwrap();
        assert!(entry_hash(&row).is_err(), "accepted {value}");
    }
    let mut changed = row.clone();
    changed
        .force_request_observation
        .as_mut()
        .unwrap()
        .audit_sequence = 56;
    assert_ne!(preimage(&changed).unwrap(), original);
    for version in [1, 2] {
        let old = AuditRow {
            hash_version: version,
            ..row.clone()
        };
        assert!(
            preimage(&old).is_err(),
            "old encoding cannot omit new evidence"
        );
    }
    assert_eq!(
        named("v3-after-v2")["input"]["prev_hash"],
        named("v2-after-v1-caller-reason")["sha256"]
    );
}
