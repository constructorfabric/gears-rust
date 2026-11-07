//! D-455's census: only [`super::transaction`] opens a [`super::TxOutbox`] and settles it, so every
//! event a door, the switch job or an approval callback enqueues wakes the outbox's sequencer after
//! its transaction commits, and never on a rollback.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use std::collections::BTreeSet;

/// The files under `src/`, outside the test modules, whose code (comments and literals blanked)
/// contains any of `needles`, relative to `src/`.
fn files_with(needles: &[&str]) -> BTreeSet<String> {
    fn walk(
        root: &std::path::Path,
        dir: &std::path::Path,
        needles: &[&str],
        out: &mut BTreeSet<String>,
    ) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                walk(root, &path, needles, out);
            } else if path.extension().is_some_and(|e| e == "rs")
                && !name.ends_with("_tests.rs")
                && name != "test_support.rs"
            {
                let code = crate::source_scan::blank_comments_and_literals(
                    &std::fs::read_to_string(&path).unwrap(),
                );
                if needles.iter().any(|needle| code.contains(needle)) {
                    let relative = path.strip_prefix(root).unwrap();
                    out.insert(relative.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = BTreeSet::new();
    walk(&root, &root, needles, &mut out);
    out
}

#[test]
fn only_the_event_transaction_opens_and_settles_a_tx_outbox() {
    let events = BTreeSet::from(["infra/events.rs".to_owned()]);
    assert_eq!(
        files_with(&["TxOutbox::new("]),
        events,
        "a door opens no TxOutbox of its own: it runs its events through events::transaction"
    );
    assert_eq!(
        files_with(&[".fire()", "Wake::fire", ".discard()", "Wake::discard"]),
        events,
        "no door fires or discards a wake itself"
    );
    let code = crate::source_scan::blank_comments_and_literals(include_str!("events.rs"));
    let transaction = code.find("pub async fn transaction<").unwrap();
    assert_eq!(code.matches("TxOutbox::new(").count(), 1);
    assert!(
        code[transaction..].contains("TxOutbox::new("),
        "the one TxOutbox::new is the event transaction's"
    );
}

/// The interim arm's envelope is the broker SDK's v1 producer-outbox envelope (PS-21), every
/// field in the shape the SDK's processor drains, and it is `stateless`, the mode the processor
/// drains without a producer registration. The SDK-shape guard lives in the broker SDK's v1 pin
/// test (`the_v1_envelope_wire_shape_is_pinned`), which decodes a copy of this body.
#[test]
fn the_interim_envelope_is_the_v1_stateless_envelope() {
    let tenant = uuid::Uuid::parse_str("5a0e8d1c-7b2f-4c6a-8e3d-9f4b1c2d3e4f").unwrap();
    let unit = uuid::Uuid::parse_str("6b1f9e2d-8c3a-4d7b-9f4e-0a5c2d3e4f5a").unwrap();
    let actor = uuid::Uuid::parse_str("7c2a0f3e-9d4b-4e8c-8a5f-1b6d3e4f5a6b").unwrap();
    let event = super::ApprovalUnitDecided {
        tenant_id: tenant,
        unit_id: unit,
        kind: "prices".into(),
        state: "approved".into(),
        generation: 1,
        actors: vec![actor],
    };
    let ours = super::interim_envelope(
        &event,
        super::APPROVAL_UNIT_SUBJECT_TYPE,
        time::OffsetDateTime::UNIX_EPOCH,
    )
    .unwrap();
    let event_id = uuid::Uuid::parse_str(ours["event_id"].as_str().unwrap()).unwrap();
    assert_eq!(event_id.get_version_num(), 7, "the event id is a UUID v7");
    assert_eq!(
        ours,
        serde_json::json!({
            "version": 1,
            "event_id": event_id.to_string(),
            "type": "gts.cf.core.events.event.v1~cf.bss.pricing.approval_unit_decided.v1~",
            "topic": "gts.cf.core.events.topic.v1~cf.bss.pricing.catalog.v1",
            "tenant_id": "5a0e8d1c-7b2f-4c6a-8e3d-9f4b1c2d3e4f",
            "source": "bss-pricing",
            "subject": "6b1f9e2d-8c3a-4d7b-9f4e-0a5c2d3e4f5a",
            "subject_type": "gts.cf.core.events.subject.v1~cf.bss.pricing.approval_unit.v1~",
            "occurred_at": "1970-01-01T00:00:00Z",
            "trace_parent": null,
            "data": {
                "tenantId": "5a0e8d1c-7b2f-4c6a-8e3d-9f4b1c2d3e4f",
                "unitId": "6b1f9e2d-8c3a-4d7b-9f4e-0a5c2d3e4f5a",
                "kind": "prices",
                "state": "approved",
                "generation": 1,
                "actors": ["7c2a0f3e-9d4b-4e8c-8a5f-1b6d3e4f5a6b"]
            },
            "producer_mode": "stateless",
            "diagnostic_metadata": { "sdk_client_agent": "bss-pricing" }
        })
    );
}
