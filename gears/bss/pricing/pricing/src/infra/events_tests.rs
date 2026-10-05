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

/// The interim arm's envelope is the broker SDK's (PS-21): it deserializes as
/// `ProducerOutboxEnvelope` and serializes back to itself, field for field, so an SDK that changes
/// its envelope fails here; and it is `stateless`, the mode the SDK's processor drains without a
/// producer registration.
#[test]
fn the_interim_envelope_is_the_sdks_stateless_envelope() {
    let event = super::ApprovalUnitDecided {
        tenant_id: uuid::Uuid::new_v4(),
        unit_id: uuid::Uuid::new_v4(),
        kind: "prices".into(),
        state: "approved".into(),
        generation: 1,
        actors: vec![uuid::Uuid::new_v4()],
    };
    let mut ours = super::interim_envelope(&event, time::OffsetDateTime::UNIX_EPOCH).unwrap();
    let sdk: event_broker_sdk::producer::ProducerOutboxEnvelope =
        serde_json::from_value(ours.clone()).unwrap();
    let back = serde_json::to_value(&sdk).unwrap();
    // The SDK leaves an absent trace parent out; ours writes it null.
    ours.as_object_mut().unwrap().remove("trace_parent");
    assert_eq!(back, ours, "every field is the SDK's, in its shape");
    assert_eq!(back["producer_mode"], "stateless");
    assert_eq!(back["version"], 1);
}
