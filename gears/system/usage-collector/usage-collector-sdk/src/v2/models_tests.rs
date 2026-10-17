//! Structural unit tests for the foundation SDK models.

use std::str::FromStr;

use serde_json::json;
use toolkit_gts::gts_id;
use uuid::Uuid;

use std::collections::BTreeMap;

use super::{
    CreateUsageRecord, EntryType, IdempotencyKey, Invalidation, MetadataKey, MeterTypeId,
    ReasonCode, RecordOrigin, ResourceRef, SubjectRef, UsageRecord, WINDOW_END_FIELD,
    WINDOW_START_FIELD,
};
use crate::v2::error::UsageCollectorError;
use crate::v2::reason::ValidationReason;

fn metadata_key(value: &str) -> MetadataKey {
    MetadataKey::new(value).expect("test fixture supplies a valid metadata key")
}

fn metadata_map<const N: usize>(entries: [(&str, &str); N]) -> BTreeMap<MetadataKey, String> {
    entries
        .into_iter()
        .map(|(k, v)| (metadata_key(k), v.to_owned()))
        .collect()
}

const SAMPLE_METER_TYPE_ID: &str =
    gts_id!("cf.core.uc.usage_record.v2~cf.mini_chat._.tokens_consumed.v1~");

fn sample_meter_id() -> MeterTypeId {
    MeterTypeId::new(SAMPLE_METER_TYPE_ID).expect("valid usage_record-derived meter type id")
}

/// `1970-01-01T00:00:00Z` — the inclusive start of the fixture period.
const SAMPLE_WINDOW_START: time::OffsetDateTime = time::OffsetDateTime::UNIX_EPOCH;
/// `1970-01-01T01:00:00Z` — one hour later, so the two bounds are distinct
/// and a test that confused them would fail rather than pass by symmetry.
const SAMPLE_WINDOW_END: time::OffsetDateTime =
    SAMPLE_WINDOW_START.saturating_add(time::Duration::hours(1));
/// `1970-01-01T01:05:00Z` — five minutes after [`SAMPLE_WINDOW_END`], so a
/// test that confused `accepted_at` with a covered-period bound would fail
/// rather than pass by coincidence.
const SAMPLE_ACCEPTED_AT: time::OffsetDateTime =
    SAMPLE_WINDOW_END.saturating_add(time::Duration::minutes(5));

/// The entry a fixture invalidation withdraws.
fn target_id() -> Uuid {
    Uuid::parse_str("33333333-3333-3333-3333-333333333333").expect("target uuid")
}

fn reason_code(value: &str) -> ReasonCode {
    ReasonCode::new(value).expect("test fixture supplies a valid reason code")
}

fn qty(s: &str) -> crate::v2::UsageQuantity {
    crate::v2::UsageQuantity::parse(s).expect("test quantity")
}

/// The idempotency key both fixtures carry, whichever kind they build.
/// A withdrawal repeats its target's key (DESIGN §3.1, `IdempotencyKey`),
/// so the two fixtures agreeing on one value is the contract, not a
/// shortcut.
fn sample_key() -> IdempotencyKey {
    IdempotencyKey::new("k-1").expect("valid idempotency key")
}

/// The reason code a fixture withdrawal states.
fn sample_reason() -> ReasonCode {
    reason_code("emitter_duplicate")
}

/// The withdrawal a fixture *entry* carries: the target the gateway
/// resolved, paired with the reason. The persisted shape is the only one
/// that holds a target — a submission carries the reason alone, because
/// `invalidates` is server-assigned.
fn sample_invalidation(target: Uuid) -> Invalidation {
    Invalidation {
        target,
        reason: sample_reason(),
    }
}

fn sample_usage_record(subject_ref: Option<SubjectRef>, invalidates: Option<Uuid>) -> UsageRecord {
    UsageRecord {
        id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").expect("record id"),
        gts_type_id: sample_meter_id(),
        tenant_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").expect("tenant uuid"),
        resource_ref: ResourceRef::new("vm-1", "compute.vm").expect("valid resource ref"),
        subject_ref,
        metadata: metadata_map([("region", "eu"), ("tier", "gold")]),
        quantity: qty("42"),
        idempotency_key: sample_key(),
        accepted_at: SAMPLE_ACCEPTED_AT,
        // `live` by default: no test that builds a record by hand has the
        // admitting path as its subject. The ones that do go through
        // `try_into_usage_record`, which is what stamps an origin — or
        // override this field after building, as the metadata cases do.
        origin: RecordOrigin::Live,
        invalidation: invalidates.map(sample_invalidation),
        window_start: SAMPLE_WINDOW_START,
        window_end: SAMPLE_WINDOW_END,
    }
}

/// A submission whose declared kind agrees with the reason code it states.
///
/// `entry_type` is caller-supplied and the two can disagree, which is a
/// caller error both projections refuse — but a disagreeing submission is
/// never what a test of something *else* wants, so this helper cannot build
/// one. The two tests that need one spell the struct out themselves.
fn sample_create_usage_record(
    subject_ref: Option<SubjectRef>,
    invalidation: Option<ReasonCode>,
) -> CreateUsageRecord {
    CreateUsageRecord {
        entry_type: if invalidation.is_some() {
            EntryType::Invalidation
        } else {
            EntryType::Record
        },
        gts_type_id: sample_meter_id(),
        tenant_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").expect("tenant uuid"),
        resource_ref: ResourceRef::new("vm-1", "compute.vm").expect("valid resource ref"),
        subject_ref,
        metadata: metadata_map([("region", "eu"), ("tier", "gold")]),
        quantity: qty("42"),
        idempotency_key: sample_key(),
        invalidation,
        window_start: SAMPLE_WINDOW_START,
        window_end: SAMPLE_WINDOW_END,
    }
}

// CreateUsageRecord::try_into_usage_record — period validation and the
// identity stamp on create

// The two projections are the single point where a submission acquires its
// identity: each validates the submission's own shape and covered period,
// stamps the deterministic derived `id`, and forwards every caller-supplied
// field verbatim. On a withdrawal the target is the one field that is not
// forwarded — it is server-assigned, so the projection takes it as an
// argument and stamps it beside the caller's reason.
#[test]
fn try_into_invalidation_record_stamps_the_derived_id_and_forwards_every_field() {
    let subject = SubjectRef::new("sub-1", Some("user".to_owned())).expect("valid subject ref");
    let input = sample_create_usage_record(Some(subject), Some(sample_reason()));

    let record = input
        .clone()
        .try_into_invalidation_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT, target_id())
        .expect("the fixture period is valid");

    // Every caller-supplied field is forwarded verbatim.
    assert_eq!(record.gts_type_id, input.gts_type_id);
    assert_eq!(record.tenant_id, input.tenant_id);
    assert_eq!(record.resource_ref, input.resource_ref);
    assert_eq!(record.subject_ref, input.subject_ref);
    assert_eq!(record.metadata, input.metadata);
    assert_eq!(record.quantity, input.quantity);
    // The key is forwarded too, on a withdrawal as on a measurement: it is
    // caller-supplied on both, and a withdrawal repeats its target's.
    assert_eq!(record.idempotency_key, sample_key());
    assert_eq!(
        record.invalidation,
        Some(Invalidation {
            target: target_id(),
            reason: sample_reason(),
        }),
        "the reason is the caller's and the target is the one the gateway handed in",
    );
    assert_eq!(record.window_start, input.window_start);
    assert_eq!(record.window_end, input.window_end);
}

/// The measurement projection refuses a submission that declares
/// `entry_type: invalidation` rather than projecting it as a record.
///
/// Projecting it would admit a withdrawal as an ordinary record — derived
/// under `entry_type = record` and so colliding with the very entry it
/// meant to withdraw. The target is server-assigned, so this projection has
/// no way to resolve one and
/// [`CreateUsageRecord::try_into_invalidation_record`] is the one that
/// takes it.
///
/// The refusal is `Internal`, not `InvalidArgument`: the submission handed
/// over here agrees with itself, so reaching this means the gateway picked
/// the projection that contradicts a body that may be faultless. A 400 here
/// would blame a caller for a host-contract breach and put an
/// implementer-facing instruction on the wire. The caller-facing half of
/// the same field is `a_record_that_states_a_reason_code_is_a_caller_error`
/// below.
#[test]
fn try_into_usage_record_refuses_a_submission_that_declares_a_withdrawal() {
    let err = sample_create_usage_record(None, Some(sample_reason()))
        .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
        .expect_err("a declared invalidation is not a measurement");
    assert!(
        matches!(err, UsageCollectorError::Internal { .. }),
        "calling the wrong projection is a host-contract breach, not caller input; got {err:?}",
    );
}

/// And the withdrawal projection refuses one that declares
/// `entry_type: record`. `Internal` on the same grounds — the gateway reads
/// the declared kind before it chooses a projection, so a measurement
/// arriving here means that reading did not happen. Its caller-facing
/// mirror is `an_invalidation_with_no_reason_code_is_a_caller_error` below.
#[test]
fn try_into_invalidation_record_refuses_a_submission_that_declares_a_record() {
    let err = sample_create_usage_record(None, None)
        .try_into_invalidation_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT, target_id())
        .expect_err("a declared measurement is not a withdrawal");
    assert!(
        matches!(err, UsageCollectorError::Internal { .. }),
        "calling the wrong projection is a host-contract breach, not caller input; got {err:?}",
    );
}

/// Two withdrawals of one record differing **only** in their reason code are
/// one dedup identity and a conflicting pair.
///
/// This is the property at-most-one-invalidation rests on
/// (`cpt-cf-usage-collector-adr-record-identity-derivation`, "Entry type is
/// an input": "Two withdrawals of the same target collide on all six
/// inputs"), and it needs both halves to hold at once. `reason_code` is not
/// one of the six, so both derive one `id`; it *is* caller-supplied, so
/// `caller_supplied_eq` is false and the store answers the second with
/// `IdempotencyConflict` rather than absorbing it — which the gateway
/// reports as `AlreadyInvalidated`. Either half alone is the wrong
/// behaviour: equal ids with equal content would silently swallow a second
/// withdrawal under a different reason, and distinct ids would let one
/// record carry two.
#[test]
fn two_withdrawals_differing_only_in_reason_are_one_identity_and_a_conflict() {
    let first = sample_create_usage_record(None, Some(reason_code("emitter_duplicate")))
        .try_into_invalidation_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT, target_id())
        .expect("the fixture period is valid");
    let second = sample_create_usage_record(None, Some(reason_code("meter_defect")))
        .try_into_invalidation_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT, target_id())
        .expect("the fixture period is valid");

    assert_ne!(
        first.invalidation, second.invalidation,
        "the two fixtures must really differ in their reason, or the rest proves nothing",
    );
    assert_eq!(
        first.id, second.id,
        "`reason_code` is not one of the six identity inputs, so two withdrawals of one record \
         reach one identity however they differ in it",
    );
    assert!(
        !first.caller_supplied_eq(&second),
        "`reason_code` IS caller-supplied, so that shared identity resolves as a conflict \
         rather than an absorbed retry",
    );
}

#[test]
fn try_into_usage_record_stamps_the_given_acceptance_instant() {
    let record = sample_create_usage_record(None, None)
        .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
        .expect("projects");
    assert_eq!(record.accepted_at, SAMPLE_ACCEPTED_AT);
    let encoded = serde_json::to_value(&record).expect("serializes");
    assert_eq!(encoded["accepted_at"], json!("1970-01-01T01:05:00Z"));
}

#[test]
fn a_submission_carrying_accepted_at_is_refused() {
    let mut body = serde_json::to_value(sample_create_usage_record(None, None)).unwrap();
    body["accepted_at"] = json!("1970-01-01T01:05:00Z");
    serde_json::from_value::<CreateUsageRecord>(body)
        .expect_err("accepted_at is server-assigned and must be refused as unknown");
}

#[test]
fn try_into_usage_record_derives_the_id_over_the_six_tuple() {
    let submission = sample_create_usage_record(None, None);
    let expected = crate::v2::id::derive_usage_record_id(
        submission.tenant_id,
        &submission.gts_type_id,
        &submission.idempotency_key,
        submission.window_start,
        submission.window_end,
        EntryType::Record,
    )
    .unwrap();
    let record = submission
        .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
        .expect("the fixture period is valid");
    assert_eq!(record.id, expected);
}

#[test]
fn try_into_invalidation_record_derives_the_id_with_the_invalidation_entry_type() {
    let submission = sample_create_usage_record(None, Some(sample_reason()));
    let expected = crate::v2::id::derive_usage_record_id(
        submission.tenant_id,
        &submission.gts_type_id,
        &submission.idempotency_key,
        submission.window_start,
        submission.window_end,
        EntryType::Invalidation,
    )
    .unwrap();
    let record = submission
        .try_into_invalidation_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT, target_id())
        .expect("the fixture period is valid");
    assert_eq!(record.id, expected);
    assert_eq!(record.entry_type(), EntryType::Invalidation);
}

/// The target the gateway has to resolve is this submission's own identity
/// inputs read with `entry_type = record`.
///
/// DESIGN §3.1, Target resolution. The withdrawal fixture repeats the
/// measurement fixture's tenant, type, key and period, so re-deriving over
/// them as a `record` must name the measurement itself — and the withdrawal
/// must land on a different identifier than the entry it withdraws.
#[test]
fn a_withdrawal_resolves_its_target_from_its_own_inputs() {
    let measurement = sample_create_usage_record(None, None)
        .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
        .expect("the fixture period is valid");

    let withdrawal_submission = sample_create_usage_record(None, Some(sample_reason()));
    let resolved_target = crate::v2::derive_usage_record_id(
        withdrawal_submission.tenant_id,
        &withdrawal_submission.gts_type_id,
        &withdrawal_submission.idempotency_key,
        withdrawal_submission.window_start,
        withdrawal_submission.window_end,
        EntryType::Record,
    )
    .unwrap();
    assert_eq!(
        resolved_target, measurement.id,
        "re-deriving a withdrawal's own inputs as a `record` must name the entry it withdraws",
    );

    let withdrawal = withdrawal_submission
        .try_into_invalidation_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT, resolved_target)
        .expect("the fixture period is valid");
    assert_ne!(
        withdrawal.id, measurement.id,
        "the entry type is the only input the two differ in, and it must move the identity",
    );
    assert_eq!(
        withdrawal.invalidation.as_ref().map(|i| i.target),
        Some(measurement.id),
    );
}

// A submission whose dedup identity matches an existing `UsageRecord`
// projects to the SAME `id` that record carries — the derivation is a pure
// function of the 6-tuple, so the create input and the persisted shape agree
// on identity without the caller ever supplying it.
#[test]
fn try_into_usage_record_id_matches_full_record_with_same_dedup_identity() {
    let input = sample_create_usage_record(None, None);
    let persisted = sample_usage_record(None, None);
    // `sample_usage_record` shares the whole dedup identity.
    assert_eq!(input.tenant_id, persisted.tenant_id);
    assert_eq!(input.gts_type_id, persisted.gts_type_id);
    assert_eq!(input.idempotency_key, persisted.idempotency_key);
    assert_eq!(input.window_start, persisted.window_start);
    assert_eq!(input.window_end, persisted.window_end);
    assert_eq!(input.entry_type(), persisted.entry_type());

    assert_eq!(
        input
            .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
            .expect("the fixture period is valid")
            .id,
        crate::v2::id::derive_usage_record_id(
            persisted.tenant_id,
            &persisted.gts_type_id,
            &persisted.idempotency_key,
            persisted.window_start,
            persisted.window_end,
            persisted.entry_type(),
        )
        .unwrap(),
        "the create-input identity must equal the derivation of the same dedup identity",
    );
}

#[test]
fn try_into_usage_record_rejects_a_sub_microsecond_window_start() {
    // `cpt-cf-usage-collector-adr-record-identity-derivation`: a bound finer
    // than the microsecond is REJECTED, not truncated. Truncating would
    // persist a period whose read-back derives
    // an id different from the one the entry carries, which breaks offline
    // reproduction at the point an emitter needs it.
    let mut submission = sample_create_usage_record(None, None);
    submission.window_start = submission.window_start.replace_nanosecond(500).unwrap();
    let err = submission
        .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
        .expect_err("sub-microsecond bound must be rejected");
    let UsageCollectorError::InvalidArgument { field, .. } = err else {
        panic!("expected InvalidArgument, got {err:?}");
    };
    assert_eq!(field, "window_start");
}

#[test]
fn try_into_usage_record_rejects_a_sub_microsecond_window_end() {
    let mut submission = sample_create_usage_record(None, None);
    submission.window_end = submission.window_end.replace_nanosecond(1).unwrap();
    let err = submission
        .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
        .expect_err("sub-microsecond bound must be rejected");
    let UsageCollectorError::InvalidArgument { field, .. } = err else {
        panic!("expected InvalidArgument, got {err:?}");
    };
    assert_eq!(field, "window_end");
}

#[test]
fn a_sub_microsecond_rejection_reads_the_same_in_every_offset() {
    // Two claims in one, both about the hoisted UTC normalization that runs
    // BEFORE the precision checks.
    //
    // Behaviour: the same instant submitted in three offsets is rejected
    // all three times — normalization moves the offset, never the
    // nanosecond, so it cannot change what the check accepts.
    //
    // Diagnostics: all three rejections echo ONE rendering. Before the
    // hoist, this bound echoed the caller's offset while the sibling
    // inverted-period error echoed UTC, and an offset carrying non-zero
    // seconds (which RFC 3339 cannot express) fell through to
    // `OffsetDateTime`'s space-separated `Display`. Comparing the details
    // to each other rather than to a literal pins the property without
    // pinning the wording.
    let sub_us = SAMPLE_WINDOW_START.replace_nanosecond(500).unwrap();
    let details: Vec<String> = [
        time::UtcOffset::UTC,
        time::UtcOffset::from_hms(5, 30, 0).unwrap(),
        time::UtcOffset::from_hms(5, 30, 30).unwrap(),
    ]
    .into_iter()
    .map(|offset| {
        let mut submission = sample_create_usage_record(None, None);
        submission.window_start = sub_us.to_offset(offset);
        let err = submission
            .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
            .expect_err("a sub-microsecond bound must be rejected in ANY offset");
        let UsageCollectorError::InvalidArgument { field, detail, .. } = err else {
            panic!("expected InvalidArgument, got {err:?}");
        };
        assert_eq!(field, WINDOW_START_FIELD);
        detail
    })
    .collect();

    assert!(
        details.windows(2).all(|pair| pair[0] == pair[1]),
        "one instant must produce one diagnostic whatever offset it arrived \
         in; got {details:#?}",
    );
    assert!(
        details[0].contains("Z,"),
        "the echoed bound must be the UTC rendering, not the caller's \
         offset; got {}",
        details[0],
    );
}

#[test]
fn try_into_usage_record_accepts_a_whole_microsecond_bound() {
    // The ceiling is the microsecond, not the millisecond or the second: a
    // bound carrying a non-zero microsecond is ordinary valid input. Without
    // this, relaxing the precondition to a coarser unit would pass every
    // other test in the file, because the fixture bounds land on whole
    // seconds.
    let mut submission = sample_create_usage_record(None, None);
    submission.window_start = submission.window_start.replace_nanosecond(1_000).unwrap();
    submission.window_end = submission
        .window_end
        .replace_nanosecond(999_999_000)
        .unwrap();
    let record = submission
        .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
        .expect("whole-microsecond bounds are valid");
    assert_eq!(record.window_start.nanosecond(), 1_000);
    assert_eq!(record.window_end.nanosecond(), 999_999_000);
}

#[test]
fn try_into_usage_record_accepts_equal_bounds_as_a_point_event() {
    let mut submission = sample_create_usage_record(None, None);
    submission.window_end = submission.window_start;
    let record = submission
        .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
        .expect("point event is valid input");
    assert_eq!(record.window_start, record.window_end);
}

#[test]
fn try_into_usage_record_rejects_an_inverted_covered_period() {
    let mut submission = sample_create_usage_record(None, None);
    submission.window_end = submission.window_start - time::Duration::seconds(1);
    let err = submission
        .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
        .expect_err("window_end < window_start must be rejected");
    let UsageCollectorError::InvalidArgument { field, .. } = err else {
        panic!("expected InvalidArgument, got {err:?}");
    };
    assert_eq!(field, "window_end");
}

#[test]
fn the_past_tolerance_rejection_names_both_backfill_surfaces() {
    // `cpt-cf-usage-collector-adr-backfill-isolation`: "The rejection names
    // the route, and both surfaces carry it." A REST caller needs the path;
    // an in-process caller needs the trait method, and a URL tells it
    // nothing. Asserted against literals rather than against
    // `BACKFILL_ROUTE_PATH`, because this string is what turns a rejection
    // into an actionable instruction: spelling the expectation from the
    // same constant the message is built from would let the path move and
    // the test still pass.
    let window_end = SAMPLE_WINDOW_END;
    let now = window_end.saturating_add(time::Duration::days(30));
    let err = UsageCollectorError::covered_period_before_past_tolerance(
        window_end,
        now,
        time::Duration::hours(48),
    );
    let UsageCollectorError::InvalidArgument {
        reason,
        field,
        detail,
        ..
    } = &err
    else {
        panic!("expected InvalidArgument, got {err:?}");
    };
    assert_eq!(*reason, ValidationReason::PastWindow);
    assert_eq!(field, WINDOW_END_FIELD);
    assert!(
        detail.contains("/usage-collector/v2/records/backfill"),
        "{detail}"
    );
    assert!(detail.contains("backfill_usage_records"), "{detail}");
    // `cpt-cf-usage-collector-fr-live-future-time-bound` requires the error
    // to identify the offending instant AND the bound it breached. The
    // tolerance is the half rendered by a third-party `Display` impl — 48
    // hours normalises to `2d` — so it is the half likelier to be mangled.
    assert!(
        detail.contains("2d"),
        "the rejection must name the bound it breached; got {detail}"
    );
}

#[test]
fn the_future_tolerance_rejection_does_not_name_the_backfill_route() {
    // The backfill route lifts the past bound and nothing else. Pointing a
    // clock-skewed emitter at it would send a defect somewhere it is just
    // as invalid, and the route's own description says so: it exists "for
    // exactly the periods that bound rejects", meaning the past one.
    let now = SAMPLE_WINDOW_END;
    let window_end = now.saturating_add(time::Duration::days(30));
    let err = UsageCollectorError::covered_period_beyond_future_tolerance(
        window_end,
        now,
        time::Duration::minutes(5),
    );
    let UsageCollectorError::InvalidArgument {
        reason,
        field,
        detail,
        ..
    } = &err
    else {
        panic!("expected InvalidArgument, got {err:?}");
    };
    assert_eq!(*reason, ValidationReason::FutureWindow);
    assert_eq!(field, WINDOW_END_FIELD);
    // The claim itself: no mention of the route, in any casing.
    assert!(!detail.to_lowercase().contains("backfill"), "{detail}");
    // A `!contains` passes vacuously against an empty detail, so anchor it
    // on what the rejection must still carry. The PRD names two halves —
    // the offending instant and the bound it breached — so pin both: the
    // instant in the RFC 3339 form the caller sent and could resubmit, and
    // the tolerance, which is rendered by a third-party `Display` impl and
    // so is the likelier of the two to be mangled.
    assert!(
        detail.contains(
            &window_end
                .format(&time::format_description::well_known::Rfc3339)
                .expect("the fixture instant is RFC 3339 representable")
        ),
        "{detail}"
    );
    assert!(
        detail.contains("5m"),
        "the rejection must name the bound it breached; got {detail}"
    );
}

#[test]
fn try_into_usage_record_normalizes_both_bounds_to_utc() {
    let offset = time::UtcOffset::from_hms(-7, 0, 0).unwrap();
    let mut submission = sample_create_usage_record(None, None);
    let start = submission.window_start;
    let end = submission.window_end;
    submission.window_start = start.to_offset(offset);
    submission.window_end = end.to_offset(offset);
    let record = submission
        .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
        .expect("the fixture period is valid");
    assert_eq!(record.window_start.offset(), time::UtcOffset::UTC);
    assert_eq!(record.window_end.offset(), time::UtcOffset::UTC);
    // Normalization moves the offset, never the instant, and it must not
    // move either bound onto the other.
    assert_eq!(record.window_start, start);
    assert_eq!(record.window_end, end);
}

#[test]
fn one_instant_in_two_offsets_derives_one_id() {
    // The retry invariant: an emitter that resends the same period in a
    // different offset must not surface a false IdempotencyConflict.
    let offset = time::UtcOffset::from_hms(2, 0, 0).unwrap();
    let utc = sample_create_usage_record(None, None)
        .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
        .expect("valid");
    let mut shifted = sample_create_usage_record(None, None);
    shifted.window_start = shifted.window_start.to_offset(offset);
    shifted.window_end = shifted.window_end.to_offset(offset);
    assert_eq!(
        utc.id,
        shifted
            .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
            .expect("valid")
            .id,
    );
}

// UsageRecord — wire shape (RFC-3339 period bounds, optional skipping)

#[test]
fn usage_record_serde_round_trip_omits_none_optionals_and_uses_rfc3339_period_bounds() {
    let record = sample_usage_record(None, None);
    let value = serde_json::to_value(&record).expect("serialize UsageRecord");
    let object = value
        .as_object()
        .expect("UsageRecord serializes as a JSON object");
    assert!(
        !object.contains_key("subject_ref"),
        "subject_ref must be omitted when None; got {object:?}"
    );
    assert!(
        !object.contains_key("invalidates"),
        "invalidates must be omitted when None; got {object:?}"
    );
    assert!(
        !object.contains_key("reason_code"),
        "reason_code must be omitted when None; got {object:?}"
    );
    assert!(
        !object.contains_key("created_at"),
        "the single instant is gone: an entry carries a covered period and \
         no other emitter-supplied time attribution; got {object:?}"
    );
    assert_eq!(
        object.get("window_start").and_then(|v| v.as_str()),
        Some("1970-01-01T00:00:00Z"),
        "window_start must serialize in RFC-3339 form with `Z` UTC marker; got {object:?}"
    );
    assert_eq!(
        object.get("window_end").and_then(|v| v.as_str()),
        Some("1970-01-01T01:00:00Z"),
        "window_end must serialize in RFC-3339 form with `Z` UTC marker; got {object:?}"
    );
    let round_tripped: UsageRecord = serde_json::from_value(value).expect("UsageRecord round-trip");
    assert_eq!(record, round_tripped);
}

#[test]
fn usage_record_deserialize_requires_both_period_bounds() {
    // Neither bound is optional and neither has a serde default: a payload
    // missing one is a malformed entry, not a point event. A `#[serde(
    // default)]` slipped onto either would silently fabricate the epoch.
    let full = serde_json::to_value(sample_usage_record(None, None)).expect("serialize");
    for missing in ["window_start", "window_end"] {
        let mut value = full.clone();
        value
            .as_object_mut()
            .expect("object")
            .remove(missing)
            .expect("fixture carries the field");
        let err = serde_json::from_value::<UsageRecord>(value)
            .expect_err("a missing period bound must be rejected");
        assert!(
            err.to_string().contains(missing),
            "deserialize error MUST name the missing bound `{missing}`; got {err}"
        );
    }
}

#[test]
fn usage_record_deserialize_rejects_an_inverted_covered_period() {
    // `CreateUsageRecord::project` rejects the inverted shape at ingestion,
    // so without this the field doc's invariant held on one of the two paths
    // an entry arrives by and was a comment on the other.
    let full = serde_json::to_value(sample_usage_record(None, None)).expect("serialize");
    let mut inverted = full.clone();
    let object = inverted.as_object_mut().expect("object");
    let start = object["window_start"].clone();
    let end = object["window_end"].clone();
    assert_ne!(start, end, "precondition: the fixture is not a point event");
    object["window_start"] = end;
    object["window_end"] = start;
    let err = serde_json::from_value::<UsageRecord>(inverted)
        .expect_err("window_end before window_start must be rejected on the wire too");
    assert!(
        err.to_string().contains("window_end"),
        "the refusal MUST name the bound at fault; got {err}"
    );

    let mut point_event = full;
    let object = point_event.as_object_mut().expect("object");
    object["window_end"] = object["window_start"].clone();
    let record = serde_json::from_value::<UsageRecord>(point_event)
        .expect("equal bounds mark a point event, not an error");
    assert_eq!(record.window_start, record.window_end);
}

#[test]
fn usage_record_serde_round_trip_carries_subject_ref_and_the_withdrawal_pair_when_some() {
    let subject = SubjectRef::new("principal-1", Some("user")).expect("valid subject ref");
    let record = sample_usage_record(Some(subject), Some(target_id()));
    let value = serde_json::to_value(&record).expect("serialize UsageRecord");
    let object = value
        .as_object()
        .expect("UsageRecord serializes as a JSON object");
    assert!(
        object.contains_key("subject_ref"),
        "subject_ref must be present when Some; got {object:?}"
    );
    assert_eq!(
        object.get("invalidates").and_then(|v| v.as_str()),
        Some("33333333-3333-3333-3333-333333333333"),
        "invalidates must serialize as a UUID string; got {object:?}"
    );
    assert_eq!(
        object.get("reason_code").and_then(|v| v.as_str()),
        Some("emitter_duplicate"),
        "reason_code must serialize transparently as its string; got {object:?}"
    );
    let round_tripped: UsageRecord = serde_json::from_value(value).expect("UsageRecord round-trip");
    assert_eq!(record, round_tripped);
}

// EntryType — derived on the persisted entry, caller-supplied on the
// submission

#[test]
fn entry_type_is_derived_from_the_reference_it_summarizes() {
    let record = sample_usage_record(None, None);
    assert_eq!(record.entry_type(), EntryType::Record);

    let invalidation = sample_usage_record(None, Some(target_id()));
    assert_eq!(invalidation.entry_type(), EntryType::Invalidation);
}

#[test]
fn an_entry_carries_no_serialized_entry_type() {
    // Derived means derived: a stored discriminator is a second place the
    // kind can be read, and the two can disagree. `deny_unknown_fields`
    // makes the negative assertion sharp — a submitted one is refused.
    // This half guards the shape a plugin returns.
    let json = serde_json::to_value(sample_usage_record(None, None)).expect("serializes");
    assert!(json.get("entry_type").is_none());

    let mut with_marker = json.as_object().expect("object").clone();
    with_marker.insert("entry_type".to_owned(), serde_json::json!("invalidation"));
    serde_json::from_value::<UsageRecord>(serde_json::Value::Object(with_marker))
        .expect_err("a submitted entry_type must be refused as an unknown field");
}

#[test]
fn a_submission_declares_its_own_entry_type() {
    // The opposite of the entry shape above, and the wire contract says so
    // in both directions: `EntryType` is "Required on every ingestion
    // request, with no default", and `CreateUsageRecordRequest` lists
    // `entry_type` among its required properties and discriminates its two
    // `oneOf` branches on it. A body conforming to the published contract
    // must therefore round-trip through this type, discriminator and all.
    let json = serde_json::to_value(sample_create_usage_record(None, None)).expect("serializes");
    assert_eq!(json.get("entry_type"), Some(&serde_json::json!("record")));

    let withdrawal = serde_json::to_value(sample_create_usage_record(None, Some(sample_reason())))
        .expect("serializes");
    assert_eq!(
        withdrawal.get("entry_type"),
        Some(&serde_json::json!("invalidation")),
    );
    assert_eq!(
        serde_json::from_value::<CreateUsageRecord>(withdrawal)
            .expect("a declared withdrawal decodes")
            .entry_type(),
        EntryType::Invalidation,
    );

    // No default: a body that omits it is refused rather than read as a
    // `record`. An invalidation stripped of its discriminator is an exact
    // copy of its target and would be absorbed as a retry, withdrawing
    // nothing.
    let mut without = json.as_object().expect("object").clone();
    without.remove("entry_type");
    serde_json::from_value::<CreateUsageRecord>(serde_json::Value::Object(without))
        .expect_err("entry_type is required on every ingestion request, with no default");
}

#[test]
fn the_codec_admits_a_disagreeing_body_and_leaves_the_refusal_to_the_projection() {
    // `CreateUsageRecordWire`'s doc claims the agreement between
    // `entry_type` and `reason_code` is deliberately not checked in the
    // codec: a `Deserialize` can only raise a plain string, and the emitter
    // is owed a typed field violation naming the field. Nothing pinned that
    // claim — every disagreement test below builds the struct directly and
    // never reaches the codec at all.
    let mut body = serde_json::to_value(sample_create_usage_record(None, Some(sample_reason())))
        .expect("serializes")
        .as_object()
        .expect("object")
        .clone();
    body.insert("entry_type".to_owned(), json!("record"));

    let decoded = serde_json::from_value::<CreateUsageRecord>(serde_json::Value::Object(body))
        .expect("the codec admits the disagreement rather than raising a serde string");
    assert_eq!(decoded.entry_type(), EntryType::Record);
    assert_eq!(decoded.invalidation, Some(sample_reason()));

    let err = decoded
        .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
        .expect_err("the projection is where a decoded disagreement is refused");
    assert!(
        matches!(
            &err,
            UsageCollectorError::InvalidArgument { field, .. } if field == "reason_code"
        ),
        "a typed field violation is the whole point of not checking in the codec; got {err:?}",
    );
}

#[test]
fn entry_type_is_read_back_rather_than_inferred_from_the_reason_code() {
    // DESIGN §3.1: the discriminator is "Never inferred from other fields".
    // The accessor has to report what the submission declared even when the
    // reason code would suggest the other kind — otherwise the two
    // disagreement refusals below could never see a disagreement to refuse.
    let contradicting = CreateUsageRecord {
        entry_type: EntryType::Record,
        ..sample_create_usage_record(None, Some(sample_reason()))
    };
    assert_eq!(contradicting.entry_type(), EntryType::Record);

    let bare = CreateUsageRecord {
        entry_type: EntryType::Invalidation,
        ..sample_create_usage_record(None, None)
    };
    assert_eq!(bare.entry_type(), EntryType::Invalidation);
}

/// A submission declaring `entry_type: invalidation` and stating no reason
/// code is refused by **both** projections, as caller input.
///
/// DESIGN §3.1, "Entry type and reason code": `reason_code` is required when
/// `entry_type` is `invalidation`. Both halves are caller-supplied, so this
/// is an emitter error and the refusal is a 400 naming `reason_code` — not
/// the `Internal` that `missing_reason_code` raises for the same absent
/// field, which reports a gateway that chose the withdrawal projection for a
/// submission declaring a `record`.
///
/// Asserting it on both projections is the point: the check runs before
/// either projection's own guard, so neither choice of projection can turn a
/// caller's mistake into a host-contract breach.
#[test]
fn an_invalidation_with_no_reason_code_is_a_caller_error() {
    let submission = CreateUsageRecord {
        entry_type: EntryType::Invalidation,
        ..sample_create_usage_record(None, None)
    };

    for err in [
        submission
            .clone()
            .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
            .expect_err("a declared withdrawal states a reason"),
        submission
            .try_into_invalidation_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT, target_id())
            .expect_err("a declared withdrawal states a reason"),
    ] {
        assert!(
            matches!(
                &err,
                UsageCollectorError::InvalidArgument { field, .. } if field == "reason_code"
            ),
            "the emitter supplied both halves, so this is their 400 on `reason_code`; got {err:?}",
        );
    }
}

/// And its mirror: a submission declaring `entry_type: record` while stating
/// a reason code, refused by both projections as caller input.
///
/// DESIGN §3.1 forbids `reason_code` on a `record`. Neither reconciliation
/// is available — dropping the reason admits a withdrawal as a measurement
/// that collides with its own target, and reading the kind off the reason is
/// the inference DESIGN forbids — so the submission is refused and the
/// emitter chooses. A 400 rather than the `Internal` of
/// `withdrawal_needs_its_target`, which fires on the declared `entry_type`
/// and attributes no field at all.
#[test]
fn a_record_that_states_a_reason_code_is_a_caller_error() {
    let submission = CreateUsageRecord {
        entry_type: EntryType::Record,
        ..sample_create_usage_record(None, Some(sample_reason()))
    };

    for err in [
        submission
            .clone()
            .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
            .expect_err("a declared measurement states no reason"),
        submission
            .try_into_invalidation_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT, target_id())
            .expect_err("a declared measurement states no reason"),
    ] {
        assert!(
            matches!(
                &err,
                UsageCollectorError::InvalidArgument { field, .. } if field == "reason_code"
            ),
            "the emitter supplied both halves, so this is their 400 on `reason_code`; got {err:?}",
        );
    }
}

#[test]
fn entry_type_as_str_matches_the_wire_spelling() {
    assert_eq!(EntryType::Record.as_str(), "record");
    assert_eq!(EntryType::Invalidation.as_str(), "invalidation");
}

#[test]
fn entry_type_all_lists_every_variant() {
    // An exhaustive match with no wildcard: adding a variant to EntryType
    // breaks this function until ALL is updated too, which is what makes
    // ALL a derived list rather than a second hand-maintained one.
    for variant in EntryType::ALL {
        match variant {
            EntryType::Record | EntryType::Invalidation => {}
        }
    }
    assert_eq!(EntryType::ALL.len(), 2, "two variants, both in ALL");
}

#[test]
fn entry_type_wire_labels_is_derived_from_all() {
    // Not hand-listed: if `wire_labels` were ever respelled as a literal
    // `["record", "invalidation"]`, this would still pass — what it
    // actually pins is that the two never drift apart, which the mutation
    // check below (`ALL` growing a variant `wire_labels` doesn't follow)
    // cannot be expressed as a unit test once `ALL` is exhaustive-checked
    // above. So this pins the current agreement, both counts and content.
    assert_eq!(
        EntryType::wire_labels(),
        &["record", "invalidation"],
        "wire_labels must list every EntryType::ALL entry's as_str(), in order"
    );
}

#[test]
fn entry_type_serde_agrees_with_as_str_on_both_variants() {
    // Two spellings of one vocabulary: `rename_all = "lowercase"` and
    // `as_str`. A surface may reach for either — the REST projection
    // serializes the value, a metric label takes the `&'static str` — so
    // they must not be able to drift. Without the rename the derived
    // `Serialize` would emit `"Record"` while `as_str` kept `"record"`,
    // and nothing would notice.
    for kind in [EntryType::Record, EntryType::Invalidation] {
        assert_eq!(
            serde_json::to_value(kind).expect("serialize EntryType"),
            json!(kind.as_str()),
            "the serde encoding of {kind:?} must be its `as_str` spelling",
        );
    }
    // Spelled out once as literals too, so the pair cannot drift together.
    assert_eq!(
        serde_json::to_string(&EntryType::Invalidation).expect("serialize"),
        "\"invalidation\"",
    );
}

#[test]
fn a_half_shape_entry_body_is_refused_on_deserialize() {
    // On the *persisted* shape the pairing is a property of the type:
    // `Invalidation` holds both halves, so an entry cannot carry one
    // without the other and there is nothing left for a projection to
    // check. A JSON body is the one place the two can still arrive apart —
    // the wire keeps them flat, per the contract — so the deserialization
    // shadow is where the rule lives. Each direction is checked separately:
    // dropping either key must be refused, and one arm covers the other's
    // shape in neither direction.
    for (present, missing) in [
        ("invalidates", "reason_code"),
        ("reason_code", "invalidates"),
    ] {
        let full =
            serde_json::to_value(sample_usage_record(None, Some(target_id()))).expect("serialize");
        let mut half = full.as_object().expect("object").clone();
        half.remove(missing).expect("the pair serializes flat");
        assert!(
            half.contains_key(present),
            "the surviving half `{present}` must still be on the body",
        );
        let err = serde_json::from_value::<UsageRecord>(serde_json::Value::Object(half))
            .expect_err("a half-shape entry body must be refused");
        assert!(
            err.to_string().contains(missing),
            "the refusal must name the missing `{missing}`; got {err}",
        );
    }
}

#[test]
fn a_submission_naming_its_own_target_is_refused_as_an_unknown_field() {
    // The ingestion shape has no half-shape to refuse, because it carries
    // no target at all: `invalidates` is server-assigned, so
    // `CreateUsageRecordRequest` publishes no such property and a submitted
    // one "is rejected as an unknown field". A caller that could name its
    // own target could withdraw a record it never measured.
    let withdrawal = serde_json::to_value(sample_create_usage_record(None, Some(sample_reason())))
        .expect("serialize");

    let mut tampered = withdrawal.clone();
    tampered
        .as_object_mut()
        .expect("object")
        .insert("invalidates".to_owned(), json!(target_id().to_string()));
    serde_json::from_value::<CreateUsageRecord>(tampered)
        .expect_err("invalidates is server-assigned and must be refused on the create shape");

    // The untampered withdrawal decodes. On the ingestion shape a
    // withdrawal is its declared `entry_type` plus the reason code it
    // states; `invalidates` is the one half it has no way to supply, which
    // is what the tampering above proves.
    let decoded = serde_json::from_value::<CreateUsageRecord>(withdrawal)
        .expect("a withdrawal declaring its kind and stating its reason is well-formed");
    assert_eq!(decoded.entry_type(), EntryType::Invalidation);
    assert_eq!(decoded.invalidation, Some(sample_reason()));
}

#[test]
fn both_entry_shapes_round_trip_through_their_own_codecs() {
    // Both shapes hand-write `Serialize` and route `Deserialize` through a
    // shadow, so the two halves of each codec are written twice and only
    // this test makes them agree. It bites in both directions because the
    // read shadows carry `deny_unknown_fields`: a key the write half
    // renames or invents is refused as unknown, and a required key it
    // stops emitting is refused as missing. Neither failure is one the
    // compiler can see — the exhaustive destructure in each `Serialize`
    // catches a *dropped* field, not a *renamed* one.
    //
    // `CreateUsageRecord` is the emitter-facing ingestion shape, so drift
    // there silently changes what an SDK client puts on the wire. It went
    // unguarded while the entry shape was covered incidentally by its
    // omit-optionals test; both are named here.
    // The empty-metadata arm is not decoration: an omitted `metadata` is
    // the common submission, it is the one field whose absence genuinely
    // needs `#[serde(default)]` on the read shadows, and the ingestion
    // shape has no other test that exercises the map-empty → key-absent →
    // default path.
    let subject = SubjectRef::new("principal-1", Some("user")).expect("valid subject ref");
    for invalidates in [None, Some(target_id())] {
        // The submission half of the same pair: an entry carrying a target
        // is the projection of a submission carrying the reason alone.
        let stated_reason = invalidates.map(|_| sample_reason());
        for subject_ref in [None, Some(subject.clone())] {
            for metadata in [metadata_map([("region", "eu")]), BTreeMap::new()] {
                let mut record = sample_usage_record(subject_ref.clone(), invalidates);
                record.metadata = metadata.clone();
                let encoded = serde_json::to_value(&record).expect("serialize UsageRecord");
                assert_eq!(
                    serde_json::from_value::<UsageRecord>(encoded.clone())
                        .unwrap_or_else(|e| panic!("UsageRecord must decode its own output: {e}")),
                    record,
                    "UsageRecord round-trip must be lossless; encoded as {encoded}",
                );

                let mut submission =
                    sample_create_usage_record(subject_ref.clone(), stated_reason.clone());
                submission.metadata = metadata;
                let encoded =
                    serde_json::to_value(&submission).expect("serialize CreateUsageRecord");
                assert_eq!(
                    serde_json::from_value::<CreateUsageRecord>(encoded.clone()).unwrap_or_else(
                        |e| { panic!("CreateUsageRecord must decode its own output: {e}") }
                    ),
                    submission,
                    "CreateUsageRecord round-trip must be lossless; encoded as {encoded}",
                );
            }
        }
    }
}

#[test]
fn the_withdrawal_pair_stays_flat_on_the_wire() {
    // The grouping is a Rust-side shape only. `usage-collector-v2.yaml`
    // declares `invalidates` and `reason_code` as two sibling properties on
    // both shapes, so a nested `invalidation` object would be a silent
    // wire break — invisible to every in-process test that round-trips
    // through the same type.
    let invalidation = serde_json::to_value(sample_usage_record(None, Some(target_id())))
        .expect("serialize an invalidation");
    let object = invalidation.as_object().expect("object");
    assert!(
        object.contains_key("invalidates") && object.contains_key("reason_code"),
        "the pair must serialize as two flat siblings; got {object:?}",
    );
    assert!(
        !object.contains_key("invalidation"),
        "the Rust grouping must not reach the wire; got {object:?}",
    );

    let submission = serde_json::to_value(sample_create_usage_record(None, Some(sample_reason())))
        .expect("serialize an invalidating submission");
    let object = submission.as_object().expect("object");
    assert!(
        object.contains_key("reason_code"),
        "the ingestion shape carries the reason code flat; got {object:?}",
    );
    assert!(
        !object.contains_key("invalidates"),
        "the ingestion shape carries no target: `invalidates` is server-assigned; got {object:?}",
    );
    assert!(!object.contains_key("invalidation"));

    for ordinary in [
        serde_json::to_value(sample_usage_record(None, None)).expect("serialize"),
        serde_json::to_value(sample_create_usage_record(None, None)).expect("serialize"),
    ] {
        let object = ordinary.as_object().expect("object");
        for absent in ["invalidates", "reason_code", "invalidation"] {
            assert!(
                !object.contains_key(absent),
                "an ordinary measurement carries no `{absent}`; got {object:?}",
            );
        }
    }
}

#[test]
fn each_entry_shape_serializes_the_exact_wire_key_set() {
    // A round-trip proves the two halves of one codec agree with each
    // other; it cannot prove either agrees with the contract. Drift applied
    // to both shadows of a type — a rename, or a changed value encoding —
    // round-trips perfectly and still breaks every client. These
    // assertions are against literals for that reason, and they are the
    // only thing standing between the wire contract and a symmetric edit.
    let record = serde_json::to_value(sample_usage_record(None, Some(target_id())))
        .expect("serialize an entry");
    let mut keys: Vec<&str> = record
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "accepted_at",
            "gts_type_id",
            "id",
            "idempotency_key",
            "invalidates",
            "metadata",
            "origin",
            "quantity",
            "reason_code",
            "resource_ref",
            "tenant_id",
            "window_end",
            "window_start",
        ],
        "the entry shape's wire key set is the contract; got {record}",
    );

    let submission = serde_json::to_value(sample_create_usage_record(None, Some(sample_reason())))
        .expect("serialize a submission");
    let mut keys: Vec<&str> = submission
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "entry_type",
            "gts_type_id",
            "idempotency_key",
            "metadata",
            "quantity",
            "reason_code",
            "resource_ref",
            "tenant_id",
            "window_end",
            "window_start",
        ],
        "the ingestion shape drops the four the server assigns - `id`, \
         `accepted_at`, `origin` and `invalidates` - and declares the one the \
         entry shape derives, `entry_type`; the key stays, because a \
         withdrawal repeats its target's; got {submission}",
    );

    // The quantity is a JSON *string*, never a JSON number, on both shapes.
    // `UsageRecord::quantity`'s own doc is where this rule is stated: a
    // number round-trips through a client's float and silently loses
    // precision. The encoding is declared twice per type now, so a
    // symmetric removal of the string-only codec would pass every
    // round-trip.
    for (shape, encoded) in [("entry", &record), ("submission", &submission)] {
        let value = encoded
            .get("quantity")
            .expect("every shape carries a quantity");
        assert_eq!(
            value,
            &json!("42"),
            "the {shape} shape must encode `quantity` as a decimal string, never a \
             JSON number; got {value}",
        );
        assert!(
            value.is_string(),
            "`quantity` must be a JSON string on {shape}"
        );
    }

    // The covered-period bounds are RFC 3339 strings on both shapes, for
    // the same reason: the encoding is declared once per shadow.
    for (shape, encoded) in [("entry", &record), ("submission", &submission)] {
        assert_eq!(
            encoded.get("window_start"),
            Some(&json!("1970-01-01T00:00:00Z")),
            "the {shape} shape must encode window_start as RFC 3339",
        );
        assert_eq!(
            encoded.get("window_end"),
            Some(&json!("1970-01-01T01:00:00Z")),
            "the {shape} shape must encode window_end as RFC 3339",
        );
    }
}

#[test]
fn the_idempotency_key_is_what_moves_the_derived_identity() {
    // The key is one of the six dedup-identity inputs, and the only
    // caller-supplied one a measurement can vary while holding its
    // attribution and period fixed. Two ordinary records differing only in
    // their key must therefore derive different ids. (The entry type, the
    // sixth input, is pinned by
    // `a_withdrawal_resolves_its_target_from_its_own_inputs` and
    // `id_tests::a_record_and_its_invalidation_derive_two_ids_from_one_key`.)
    let target = sample_create_usage_record(None, None);

    let mut rekeyed = target.clone();
    rekeyed.idempotency_key = IdempotencyKey::new("a-different-key").expect("valid");
    assert_ne!(
        target
            .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
            .expect("record")
            .id,
        rekeyed
            .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
            .expect("re-keyed")
            .id,
        "the idempotency key is the one departure that does move it",
    );
}

// ReasonCode — validated construction and serde routing

#[test]
fn reason_code_rejects_the_wire_bounds_and_control_characters() {
    // The length bounds are the wire contract's `ReasonCode` schema
    // (`minLength: 1`, `maxLength: 128`), read as characters — the same
    // reading `IdempotencyKey` gives its own 256.
    ReasonCode::new("").expect_err("empty");
    ReasonCode::new("x".repeat(129)).expect_err("over 128 characters");
    // The cap counts characters, as the wire schema's `maxLength` does.
    ReasonCode::new("\u{e9}".repeat(129)).expect_err("129 characters");
    // The schema states no pattern for a reason code, so the control
    // character exclusion is this newtype's own, and stricter: wire
    // hygiene for a value that reaches a plugin, not pre-image safety —
    // the code is no input to the identity derivation.
    ReasonCode::new("emitter\u{7f}duplicate").expect_err("DEL is a control character");
    ReasonCode::new("emitter\u{1f}duplicate").expect_err("0x1F is a control character");
    ReasonCode::new("x".repeat(128)).expect("128 characters is the boundary, inclusive");
    ReasonCode::new("emitter_duplicate").expect("an ordinary code");
}

#[test]
fn reason_code_rejects_a_whitespace_only_code() {
    // `cpt-cf-usage-collector-dod-mandatory-reason-code` and
    // `cpt-cf-usage-collector-algo-entry-type-discrimination` require an
    // absent, empty **or whitespace-only** code rejected.
    ReasonCode::new(" ").expect_err("a space states no reason");
    ReasonCode::new("\t\n").expect_err("nor do a tab and a newline");
    ReasonCode::new("\u{3000}").expect_err("an ideographic space is Unicode whitespace");
    // Only an *entirely* whitespace code is refused; nothing trims the rest.
    ReasonCode::new(" emitter duplicate ").expect("content survives, spaces and all");
}

#[test]
fn reason_code_deserialize_routes_through_new() {
    let err = serde_json::from_value::<ReasonCode>(json!(""))
        .expect_err("empty code must surface as a serde error");
    assert!(
        err.to_string().contains("reason_code must not be empty"),
        "serde error must carry the Validation detail; got {err}"
    );
}

#[test]
fn reason_code_serializes_transparently() {
    let code = reason_code("emitter_duplicate");
    assert_eq!(
        serde_json::to_value(&code).expect("serialize"),
        json!("emitter_duplicate")
    );
    assert_eq!(code.as_str(), "emitter_duplicate");
    assert_eq!(code.to_string(), "emitter_duplicate");
}

#[test]
fn reason_code_from_str_routes_through_new() {
    assert!(ReasonCode::from_str("emitter_duplicate").is_ok());
    let err = ReasonCode::from_str("").expect_err("empty code must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidArgument { ref field, .. } if field.as_str() == "reason_code"
    ));
}

// MetadataFilter — validated construction and wire shape

// AggregationDimension — wire shapes

// AggregationResult / AggregationBucket — wire shape

// MetadataKey — validated construction and serde routing

#[test]
fn metadata_key_new_accepts_well_formed_string() {
    let key = MetadataKey::new("region").expect("valid key");
    assert_eq!(key.as_str(), "region");
    assert_eq!(key.to_string(), "region");
}

#[test]
fn metadata_key_new_rejects_empty_string() {
    let err = MetadataKey::new("").expect_err("empty key must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidArgument { ref field, .. } if field.as_str() == "metadata"
    ));
}

#[test]
fn metadata_key_new_rejects_nul_byte() {
    let err = MetadataKey::new("bad\0key").expect_err("NUL byte must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidArgument { ref field, .. } if field.as_str() == "metadata"
    ));
}

#[test]
fn metadata_key_new_rejects_a_key_past_the_character_cap() {
    // Characters, as every sibling cap in this file counts.
    MetadataKey::new("k".repeat(129)).expect_err("over 128 characters");
    MetadataKey::new("\u{e9}".repeat(129)).expect_err("129 characters, not 258 bytes");
    MetadataKey::new("k".repeat(128)).expect("128 characters is the boundary, inclusive");
}

#[test]
fn metadata_key_new_rejects_ascii_control_characters() {
    // A key reaches storage as a `jsonb` object key and comes back as an
    // `r.metadata ->> $N` bind, so it carries the exclusion `IdempotencyKey`
    // and `MeterTypeId` already do — NUL among the characters it covers.
    MetadataKey::new("re\u{1f}gion").expect_err("0x1F is a control character");
    MetadataKey::new("re\u{7f}gion").expect_err("DEL is a control character");
    MetadataKey::new("re\ngion").expect_err("newline is a control character");
}

#[test]
fn metadata_key_deserialize_routes_through_new() {
    let err = serde_json::from_value::<MetadataKey>(json!(""))
        .expect_err("empty key must surface as a serde error");
    assert!(err.to_string().contains("metadata key must not be empty"));
}

#[test]
fn metadata_key_serializes_transparently() {
    let key = MetadataKey::new("region").expect("valid key");
    let value = serde_json::to_value(&key).expect("serialize");
    assert_eq!(value, json!("region"));
}

// UsageRecord.metadata wire shape (BTreeMap<MetadataKey, String>)

#[test]
fn usage_record_metadata_serializes_as_string_to_string_map() {
    let record = sample_usage_record(None, None);
    let value = serde_json::to_value(&record).expect("serialize UsageRecord");
    let metadata = value
        .get("metadata")
        .expect("metadata present when non-empty");
    assert_eq!(metadata, &json!({"region": "eu", "tier": "gold"}));
}

#[test]
fn usage_record_metadata_omitted_from_wire_when_empty() {
    let mut record = sample_usage_record(None, None);
    record.metadata = BTreeMap::new();
    let value = serde_json::to_value(&record).expect("serialize UsageRecord");
    assert!(
        value.get("metadata").is_none(),
        "empty metadata map must be skipped on the wire; got {value}"
    );
}

#[test]
fn usage_record_metadata_deserialize_rejects_non_string_value() {
    let mut value =
        serde_json::to_value(sample_usage_record(None, None)).expect("serialize seed UsageRecord");
    value
        .as_object_mut()
        .expect("object")
        .insert("metadata".to_owned(), json!({"region": 42}));
    let err = serde_json::from_value::<UsageRecord>(value)
        .expect_err("non-string metadata value must be rejected at the type boundary");
    assert!(err.to_string().contains("invalid type"));
}

#[test]
fn usage_record_metadata_deserialize_defaults_to_empty_when_missing() {
    let mut value =
        serde_json::to_value(sample_usage_record(None, None)).expect("serialize seed UsageRecord");
    value.as_object_mut().expect("object").remove("metadata");
    let decoded: UsageRecord = serde_json::from_value(value)
        .expect("UsageRecord without metadata field deserializes via #[serde(default)]");
    assert!(decoded.metadata.is_empty());
}

#[test]
fn usage_record_rejects_unknown_fields() {
    let mut value =
        serde_json::to_value(sample_usage_record(None, None)).expect("serialize seed UsageRecord");
    value
        .as_object_mut()
        .expect("object")
        .insert("legacy_schema_field".to_owned(), json!({"type": "object"}));
    let err = serde_json::from_value::<UsageRecord>(value)
        .expect_err("unknown field must be rejected at the wire boundary");
    assert!(err.to_string().contains("unknown field"));
}

// IdempotencyKey — validated construction + serde routing

#[test]
fn idempotency_key_new_accepts_non_empty_string() {
    let k = IdempotencyKey::new("idem-1").expect("valid key");
    assert_eq!(k.as_str(), "idem-1");
    assert_eq!(k.to_string(), "idem-1");
}

#[test]
fn idempotency_key_new_rejects_empty_string() {
    let err = IdempotencyKey::new("").expect_err("empty key must be rejected");
    assert!(
        matches!(err, UsageCollectorError::InvalidArgument { ref field, .. } if field.as_str() == "idempotency_key"),
        "expected InvalidIdempotencyKey, got {err:?}"
    );
}

#[test]
fn idempotency_key_new_rejects_nul_byte() {
    let err = IdempotencyKey::new("bad\0key").expect_err("NUL byte must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidArgument { ref field, .. } if field.as_str() == "idempotency_key"
    ));
}

#[test]
fn idempotency_key_rejects_a_unit_separator_del_and_an_over_long_key() {
    // A confirmation of
    // `cpt-cf-usage-collector-adr-record-identity-derivation`: the derivation
    // concatenates the key under a
    // 0x1F separator and the key is not the final field, so a key carrying
    // that byte would inject a separator mid-pre-image. Rejected at
    // construction, before any derivation can run. DEL and the 256-byte
    // ceiling come from the same wire schema pattern.
    IdempotencyKey::new("idem\u{1f}1").expect_err("0x1F in a key must be rejected");
    IdempotencyKey::new("idem\u{7f}1").expect_err("DEL in a key must be rejected");
    IdempotencyKey::new("a".repeat(257)).expect_err("over-long key must be rejected");
    IdempotencyKey::new("a".repeat(256)).expect("256 bytes is the ceiling, not past it");
}

#[test]
fn idempotency_key_serializes_transparently() {
    let k = IdempotencyKey::new("idem-1").expect("valid key");
    let value = serde_json::to_value(&k).expect("serialize");
    assert_eq!(value, json!("idem-1"));
}

#[test]
fn idempotency_key_deserialize_routes_through_new() {
    let err = serde_json::from_value::<IdempotencyKey>(json!(""))
        .expect_err("empty key must surface as a serde error");
    assert!(
        err.to_string()
            .contains("idempotency_key must not be empty"),
        "serde error must carry the Validation detail; got {err}"
    );
}

#[test]
fn idempotency_key_from_str_routes_through_new() {
    assert!(IdempotencyKey::from_str("idem-1").is_ok());
    let err = IdempotencyKey::from_str("").expect_err("empty key must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidArgument { ref field, .. } if field.as_str() == "idempotency_key"
    ));
}

#[test]
fn no_key_prefix_is_reserved() {
    // The `inv:` prefix went with the derived invalidation key
    // (`cpt-cf-usage-collector-adr-record-identity-derivation`: an
    // invalidation repeats its target's caller-supplied key). Nothing
    // reserves any part of the key space now, so a key that happens to
    // begin `inv:` is an ordinary key on every constructor and on the
    // `Deserialize` that routes through `new`.
    assert_eq!(
        IdempotencyKey::new("inv:anything")
            .expect("no prefix is reserved")
            .as_str(),
        "inv:anything",
    );
    assert_eq!(
        serde_json::from_value::<IdempotencyKey>(json!("inv:x"))
            .expect("deserialize routes through new, which reserves nothing")
            .as_str(),
        "inv:x",
    );
}

#[test]
fn from_stored_applies_exactly_the_checks_new_does() {
    // The two diverged only over the reserved prefix, and `from_stored`
    // now delegates to `new`. What it still carries is intent: it names a
    // rehydration rather than an admission.
    assert_eq!(
        IdempotencyKey::from_stored("k-1")
            .expect("a stored key")
            .as_str(),
        "k-1",
    );
    IdempotencyKey::from_stored("").expect_err("still non-empty");
    IdempotencyKey::from_stored("a\u{1f}b").expect_err("still control-character free");
    IdempotencyKey::from_stored("x".repeat(257)).expect_err("still capped at 256 characters");
}

#[test]
fn an_invalidation_is_stored_under_the_key_it_was_submitted_with() {
    // The whole of DESIGN §3.1's Target resolution rests on this: the key
    // reaches the store unchanged, so the identity the gateway derived for
    // the target is reproducible from the entry that withdrew it.
    let record = sample_create_usage_record(None, Some(sample_reason()))
        .try_into_invalidation_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT, target_id())
        .expect("projects");
    assert_eq!(record.idempotency_key, sample_key());
    assert_eq!(
        record.id,
        crate::v2::derive_usage_record_id(
            record.tenant_id,
            &record.gts_type_id,
            &sample_key(),
            record.window_start,
            record.window_end,
            EntryType::Invalidation,
        )
        .unwrap(),
    );
}

// ResourceRef — validated construction + serde routing

#[test]
fn resource_ref_new_accepts_non_empty_components() {
    let r = ResourceRef::new("vm-1", "compute.vm").expect("valid ref");
    assert_eq!(r.resource_id(), "vm-1");
    assert_eq!(r.resource_type(), "compute.vm");
}

#[test]
fn resource_ref_new_rejects_empty_resource_id() {
    let err = ResourceRef::new("", "compute.vm").expect_err("empty resource_id must be rejected");
    assert!(
        matches!(err, UsageCollectorError::InvalidArgument { ref field, .. } if field.as_str() == "resource_ref.resource_id"),
        "expected InvalidResourceRef, got {err:?}"
    );
}

#[test]
fn resource_ref_new_rejects_empty_resource_type() {
    let err = ResourceRef::new("vm-1", "").expect_err("empty resource_type must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidArgument { ref field, .. } if field.as_str() == "resource_ref.resource_type"
    ));
}

#[test]
fn resource_ref_new_rejects_nul_byte_in_resource_id() {
    let err = ResourceRef::new("vm\0bad", "compute.vm")
        .expect_err("NUL byte in resource_id must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidArgument { ref field, .. } if field.as_str() == "resource_ref.resource_id"
    ));
}

#[test]
fn resource_ref_new_rejects_nul_byte_in_resource_type() {
    let err = ResourceRef::new("vm-1", "compute\0vm")
        .expect_err("NUL byte in resource_type must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidArgument { ref field, .. } if field.as_str() == "resource_ref.resource_type"
    ));
}

#[test]
fn resource_ref_new_rejects_ascii_control_characters_in_either_component() {
    // Both components are admissible `$orderby` keys, so each rides a page's
    // keyset, where a control character costs six JSON bytes against the one
    // it costs in the column — see
    // `no_admissible_attribution_value_can_outgrow_the_keyset_budget` below.
    ResourceRef::new("vm\u{1f}1", "compute.vm").expect_err("0x1F in resource_id");
    ResourceRef::new("vm-1", "compute\u{1f}vm").expect_err("0x1F in resource_type");
    ResourceRef::new("vm\u{7f}1", "compute.vm").expect_err("DEL in resource_id");
    ResourceRef::new("vm-1", "compute\nvm").expect_err("newline in resource_type");
}

#[test]
fn subject_ref_new_rejects_ascii_control_characters_in_either_component() {
    // Neither component is a keyset key — both are domain-optional — but
    // both reach the same columns, so they carry `ResourceRef`'s exclusion.
    SubjectRef::new("user\u{1f}1", None::<String>).expect_err("0x1F in subject_id");
    SubjectRef::new("user-1", Some("iam\u{1f}user")).expect_err("0x1F in subject_type");
    SubjectRef::new("user\u{7f}1", None::<String>).expect_err("DEL in subject_id");
}

#[test]
fn no_admissible_attribution_value_can_outgrow_the_keyset_budget() {
    // `cap_attribution` bounds the *character* count and `Keyset::new` the
    // JSON-encoded *byte* count, so what matters is the widest encoding a
    // capped value reaches. `MAX_KEYSET_BYTES` is derived against 4-byte
    // scalars at the cap (`keyset_tests`); a control character costs six
    // bytes against that four, so admitting one puts the widest legal keyset
    // past its own budget.
    let control_at_cap: String = std::iter::repeat_n('\u{1f}', 256).collect();
    ResourceRef::new(control_at_cap.clone(), "compute.vm")
        .expect_err("the only attribution value that could outgrow the budget is refused");
    assert!(
        serde_json::to_string(&[&control_at_cap, &control_at_cap])
            .expect("values always serialize")
            .len()
            > crate::v2::keyset::MAX_KEYSET_BYTES,
        "precondition: two such values alone exceed the whole cursor budget, so admitting \
         one is a 500 on a later read rather than a tidiness question"
    );

    // And nothing still admissible encodes wider: `serde_json` escapes only
    // `"`, `\` and the ASCII controls, the first two at 2 bytes against 4.
    let four_byte_at_cap: String = std::iter::repeat_n('\u{1F600}', 256).collect();
    let widest = ResourceRef::new(four_byte_at_cap.clone(), four_byte_at_cap)
        .expect("256 four-byte characters is at the character cap, not past it");
    let widest_len = serde_json::to_string(&[widest.resource_id(), widest.resource_type()])
        .expect("values always serialize")
        .len();
    assert_eq!(
        widest_len, 2055,
        "two attribution values at the cap encode to 2048 bytes plus 7 of array framing"
    );
    let quote_at_cap: String = std::iter::repeat_n('"', 256).collect();
    let escaped = ResourceRef::new(quote_at_cap.clone(), quote_at_cap)
        .expect("a quote is not a control character");
    assert!(
        serde_json::to_string(&[escaped.resource_id(), escaped.resource_type()])
            .expect("values always serialize")
            .len()
            < widest_len,
        "JSON escaping of an admissible character cannot beat a 4-byte scalar"
    );
}

#[test]
fn resource_ref_serde_round_trips_wire_shape() {
    let r = ResourceRef::new("vm-1", "compute.vm").expect("valid ref");
    let value = serde_json::to_value(&r).expect("serialize");
    assert_eq!(
        value,
        json!({"resource_id": "vm-1", "resource_type": "compute.vm"}),
        "wire shape must be {{resource_id, resource_type}}; got {value}"
    );
    let decoded: ResourceRef = serde_json::from_value(value).expect("round-trip");
    assert_eq!(decoded, r);
}

#[test]
fn resource_ref_deserialize_routes_through_new() {
    let err = serde_json::from_value::<ResourceRef>(json!({
        "resource_id": "",
        "resource_type": "compute.vm",
    }))
    .expect_err("empty resource_id must surface as a serde error");
    assert!(
        err.to_string().contains("resource_id must not be empty"),
        "serde error must carry the Validation detail; got {err}"
    );

    let err = serde_json::from_value::<ResourceRef>(json!({
        "resource_id": "vm-1",
        "resource_type": "",
    }))
    .expect_err("empty resource_type must surface as a serde error");
    assert!(err.to_string().contains("resource_type must not be empty"));
}

#[test]
fn resource_ref_deserialize_rejects_unknown_fields() {
    let err = serde_json::from_value::<ResourceRef>(json!({
        "resource_id": "vm-1",
        "resource_type": "compute.vm",
        "tenant_scope": "main",
    }))
    .expect_err("unknown field must be rejected at the wire boundary");
    assert!(err.to_string().contains("unknown field"));
}

// SubjectRef — validated construction + serde routing

#[test]
fn subject_ref_new_accepts_subject_id_only() {
    let s = SubjectRef::new("principal-1", Option::<&str>::None).expect("valid ref");
    assert_eq!(s.subject_id(), "principal-1");
    assert!(s.subject_type().is_none());
}

#[test]
fn subject_ref_new_accepts_subject_id_and_subject_type() {
    let s = SubjectRef::new("principal-1", Some("user")).expect("valid ref");
    assert_eq!(s.subject_id(), "principal-1");
    assert_eq!(s.subject_type(), Some("user"));
}

#[test]
fn subject_ref_new_rejects_empty_subject_id() {
    let err = SubjectRef::new("", Some("user")).expect_err("empty subject_id must be rejected");
    assert!(
        matches!(err, UsageCollectorError::InvalidArgument { ref field, .. } if field.as_str() == "subject_ref.subject_id")
    );
}

#[test]
fn subject_ref_new_rejects_explicit_empty_subject_type() {
    let err = SubjectRef::new("principal-1", Some(""))
        .expect_err("Some(\"\") subject_type must be rejected");
    assert!(
        matches!(err, UsageCollectorError::InvalidArgument { ref field, .. } if field.as_str() == "subject_ref.subject_type")
    );
}

#[test]
fn subject_ref_new_rejects_nul_byte_in_subject_id() {
    let err = SubjectRef::new("principal\0bad", Some("user"))
        .expect_err("NUL byte in subject_id must be rejected");
    assert!(
        matches!(err, UsageCollectorError::InvalidArgument { ref field, .. } if field.as_str() == "subject_ref.subject_id")
    );
}

#[test]
fn subject_ref_new_rejects_nul_byte_in_subject_type() {
    let err = SubjectRef::new("principal-1", Some("user\0bad"))
        .expect_err("NUL byte in subject_type must be rejected");
    assert!(
        matches!(err, UsageCollectorError::InvalidArgument { ref field, .. } if field.as_str() == "subject_ref.subject_type")
    );
}

#[test]
fn subject_ref_serde_round_trips_omitting_none_subject_type() {
    let s = SubjectRef::new("principal-1", Option::<&str>::None).expect("valid ref");
    let value = serde_json::to_value(&s).expect("serialize");
    assert_eq!(
        value,
        json!({"subject_id": "principal-1"}),
        "subject_type must be omitted when None; got {value}"
    );
    let decoded: SubjectRef = serde_json::from_value(value).expect("round-trip");
    assert_eq!(decoded, s);
}

#[test]
fn subject_ref_serde_round_trips_carrying_subject_type() {
    let s = SubjectRef::new("principal-1", Some("user")).expect("valid ref");
    let value = serde_json::to_value(&s).expect("serialize");
    assert_eq!(
        value,
        json!({"subject_id": "principal-1", "subject_type": "user"}),
    );
    let decoded: SubjectRef = serde_json::from_value(value).expect("round-trip");
    assert_eq!(decoded, s);
}

#[test]
fn subject_ref_deserialize_routes_through_new() {
    let err = serde_json::from_value::<SubjectRef>(json!({
        "subject_id": "",
        "subject_type": "user",
    }))
    .expect_err("empty subject_id must surface as a serde error");
    assert!(
        err.to_string()
            .contains("subject_ref.subject_id must not be empty")
    );

    let err = serde_json::from_value::<SubjectRef>(json!({
        "subject_id": "principal-1",
        "subject_type": "",
    }))
    .expect_err("empty subject_type must surface as a serde error");
    assert!(
        err.to_string()
            .contains("subject_ref.subject_type must not be empty")
    );
}

#[test]
fn subject_ref_deserialize_rejects_unknown_fields() {
    let err = serde_json::from_value::<SubjectRef>(json!({
        "subject_id": "principal-1",
        "subject_type": "user",
        "tenant_id": "00000000-0000-0000-0000-000000000000",
    }))
    .expect_err("unknown field must be rejected at the wire boundary");
    assert!(err.to_string().contains("unknown field"));
}

// UsageRecordQuery — OData filter surface
//
// `gts_type_id` is carried as a typed parameter on `list_usage_records` /
// `query_aggregated_usage_records`. The OData filter surface declared by
// `UsageRecordQuery` deliberately omits it so that
// `parse_odata_filter::<UsageRecordFilterField>` rejects any
// `gts_type_id`-touching predicate at parse time — implementations and the
// gateway do not need a runtime reject path.

// Keyset-safe (never-null) order-field classification

#[test]
fn the_duplicate_order_key_refusal_names_the_repeated_key_and_the_cursor_bound() {
    // `keyset_defect` refuses a repeated `$orderby` key on the ground
    // of the published cursor budget, not a claimed order-uniqueness rule —
    // no document states one — so the detail must say so rather than
    // invent a semantic justification a later reader could cite as if it
    // were published.
    let err = UsageCollectorError::duplicate_order_key("resource_id");
    let UsageCollectorError::InvalidArgument {
        ref field,
        ref detail,
        ..
    } = err
    else {
        panic!("expected InvalidArgument, got {err:?}");
    };
    assert_eq!(field, "$orderby");
    assert!(
        detail.contains("resource_id"),
        "the refusal must echo the key the caller repeated; got {detail}",
    );
    assert!(
        detail.contains("more than once"),
        "the refusal must say the key was repeated, not merely inadmissible; got {detail}",
    );
    assert!(
        detail.contains("4096") || detail.contains("cursor"),
        "the ground is the published cursor bound, so the detail must name the budget rather \
         than an unstated uniqueness rule; got {detail}",
    );
}

#[test]
fn the_unsupported_cursor_direction_refusal_does_not_misquote_the_keyset_wrapper() {
    // Reusing `inadmissible_cursor_keyset` here would say "the
    // cursor's bound order is not a usable keyset" — false of a `d: "bwd"`
    // token, whose bound order is fine; only the direction is unsupported.
    // `unsupported_cursor_direction` is the sibling that exists so the
    // detail says the true thing. Pinned the same way
    // `the_duplicate_order_key_refusal_names_the_repeated_key_and_the_cursor_bound`
    // pins `duplicate_order_key`'s, so a future edit cannot revert this
    // constructor to the keyset wording without reding a test.
    let err = UsageCollectorError::unsupported_cursor_direction(
        "it specifies direction `bwd`; the raw path reads forward only",
    );
    let UsageCollectorError::CursorRejected {
        ref source,
        ref detail,
        ..
    } = err
    else {
        panic!("expected CursorRejected, got {err:?}");
    };
    assert!(
        matches!(source, toolkit_odata::Error::InvalidCursor),
        "the wire code still comes from upstream's InvalidCursor; got {source:?}",
    );
    assert!(
        !detail.contains("bound order") && !detail.contains("usable keyset"),
        "this refusal must not claim the order is unusable — only the direction is; got {detail}",
    );
    assert!(
        detail.contains("direction"),
        "the refusal must name what is actually wrong; got {detail}",
    );
    assert!(
        detail.contains("bwd"),
        "the refusal must echo the defect it was given; got {detail}",
    );
}

// AggregationFold — declared-fold serde/FromStr surface

// MeterTypeId — construction validation, serde routing, FromStr, Display

const VALID_METER: &str = "gts.cf.core.uc.usage_record.v2~example.metering._.stored_volume.v1~";

#[test]
fn meter_type_id_accepts_a_single_derivation_of_the_base() {
    let id = MeterTypeId::new(VALID_METER).expect("valid meter type id");
    assert_eq!(id.as_str(), VALID_METER);
}

#[test]
fn meter_type_id_rejects_the_bare_base_type() {
    // The base is abstract. A meter must add exactly one segment.
    assert!(MeterTypeId::new("gts.cf.core.uc.usage_record.v2~").is_err());
}

#[test]
fn meter_type_id_rejects_a_type_outside_the_base() {
    assert!(MeterTypeId::new("gts.cf.core.uc.usage_type.v1~foo.bar._.baz.v1~").is_err());
}

#[test]
fn meter_type_id_rejects_a_missing_terminator() {
    // No trailing `~` makes it an instance id, not a type id.
    assert!(
        MeterTypeId::new("gts.cf.core.uc.usage_record.v2~example.metering._.stored_volume.v1")
            .is_err()
    );
}

// A meter is a leaf: exactly one derivation segment on top of the base. A
// second segment (`base~mid.v1~tail.v1~`, a two-level chain) has the right
// prefix and the right terminator, so only the interior-`~` check inside the
// stripped segment catches it — this is the case a plain `strip_prefix`
// check would let through.
#[test]
fn meter_type_id_rejects_a_deep_derivation_chain() {
    let err = MeterTypeId::new("gts.cf.core.uc.usage_record.v2~a.b._.c.v1~d.e._.f.v1~")
        .expect_err("a meter is a leaf; a second derivation segment must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidArgument {
            reason: ValidationReason::InvalidBaseGtsId,
            ..
        }
    ));
}

// Empty inner segment (consecutive `~`) must also be rejected: stripping the
// base prefix leaves the bare terminator `~` with nothing before it, and the
// `segment.is_empty()` check after stripping that trailing `~` is what
// catches it.
#[test]
fn meter_type_id_rejects_consecutive_tildes() {
    let err = MeterTypeId::new("gts.cf.core.uc.usage_record.v2~~")
        .expect_err("consecutive tildes (empty derivation segment) must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidArgument {
            reason: ValidationReason::InvalidBaseGtsId,
            ..
        }
    ));
}

#[test]
fn meter_type_id_rejects_control_characters() {
    // `cpt-cf-usage-collector-adr-record-identity-derivation` concatenates
    // this value under a 0x1F separator, so a control character would break
    // the injectivity the identifier derivation needs.
    let with_us = "gts.cf.core.uc.usage_record.v2~exa\u{1F}mple._.m.v1~";
    assert!(MeterTypeId::new(with_us).is_err());
    let with_del = "gts.cf.core.uc.usage_record.v2~exa\u{7F}mple._.m.v1~";
    assert!(MeterTypeId::new(with_del).is_err());
}

#[test]
fn meter_type_id_caps_a_well_formed_identifier_at_512_bytes() {
    // The segment is five dot-separated tokens, so only the cap can reject it:
    // a padding token short enough to fit leaves an accepted id, one byte
    // longer does not. `{BASE}{a}.{b}.{c}.{pad}.v1~` is 31 + pad + 10 bytes.
    let id = |pad: usize| {
        format!(
            "gts.cf.core.uc.usage_record.v2~a.b.c.{}.v1~",
            "x".repeat(pad)
        )
    };

    let at_cap = id(471);
    assert_eq!(at_cap.len(), 512, "fixture must sit exactly on the cap");
    MeterTypeId::new(&at_cap).expect("512 bytes is the ceiling, not past it");

    let over = id(472);
    assert_eq!(over.len(), 513);
    let err = MeterTypeId::new(&over).expect_err("513 bytes is over the cap");
    assert!(
        err.to_string().contains("must be at most 512 bytes"),
        "the cap must be the rule that fires, not the GTS grammar; got {err}"
    );
}

/// The cap counts bytes, unlike the attribution and idempotency caps, which
/// count characters.
///
/// Nothing valid is lost by it: a GTS segment token is `[a-z_][a-z0-9_]*`, so
/// no multi-byte identifier reaches the grammar at all. What the byte count
/// decides is only which rejection a caller gets, and it is the cap, because
/// the length check runs before the parse.
#[test]
fn the_meter_type_id_cap_counts_bytes_and_fires_before_the_grammar() {
    let multibyte = format!(
        "gts.cf.core.uc.usage_record.v2~a.b.c.{}.v1~",
        "\u{e9}".repeat(300)
    );
    assert_eq!(
        multibyte.chars().count(),
        341,
        "under the cap in characters"
    );
    let err = MeterTypeId::new(multibyte).expect_err("600 bytes is over the cap");
    assert!(
        err.to_string().contains("must be at most 512 bytes"),
        "got {err}"
    );
}

#[test]
fn meter_type_id_deserialize_routes_through_validation() {
    let bad = serde_json::json!("gts.cf.core.uc.usage_record.v2~");
    assert!(serde_json::from_value::<MeterTypeId>(bad).is_err());

    let good = serde_json::json!(VALID_METER);
    let parsed: MeterTypeId = serde_json::from_value(good).expect("valid");
    assert_eq!(parsed.as_str(), VALID_METER);
}

#[test]
fn meter_type_id_rejects_control_characters_as_validation_error_naming_the_value() {
    // Pins the concrete variant, the attributed field, and that `detail`
    // names the offending value rather than a generic GTS parse error —
    // matching usage_kind_from_str_rejects_unknown_variant_as_validation_error.
    // The value is echoed escaped: this branch rejects a value that still
    // carries its control characters, and `detail` reaches the log.
    let with_us = "gts.cf.core.uc.usage_record.v2~exa\u{1F}mple._.m.v1~";
    let echoed = "gts.cf.core.uc.usage_record.v2~exa\\u{1f}mple._.m.v1~";
    let err = MeterTypeId::new(with_us).expect_err("control character must be rejected");
    assert!(
        matches!(
            err,
            UsageCollectorError::InvalidArgument {
                ref field,
                ref reason,
                ref detail,
                ..
            } if field == "gts_type_id"
                && matches!(reason, ValidationReason::InvalidBaseGtsId)
                && detail.contains(echoed)
                && !detail.contains('\u{1F}')
        ),
        "expected InvalidArgument[gts_type_id/InvalidBaseGtsId] naming the escaped value, got {err:?}"
    );
}

#[test]
fn meter_type_id_as_str_and_display_match_the_wire_string() {
    let id = MeterTypeId::new(VALID_METER).expect("valid meter type id");
    assert_eq!(id.as_str(), VALID_METER);
    assert_eq!(id.to_string(), VALID_METER);
}

#[test]
fn meter_type_id_from_str_routes_through_new() {
    let id: MeterTypeId = VALID_METER
        .parse()
        .expect("valid meter type id via FromStr");
    assert_eq!(id.as_str(), VALID_METER);

    let err = "gts.cf.core.uc.usage_record.v2~"
        .parse::<MeterTypeId>()
        .expect_err("bare base must be rejected by FromStr");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidArgument { ref field, .. } if field == "gts_type_id"
    ));
}

#[test]
fn record_origin_wire_spellings_are_the_two_the_contract_declares() {
    // `RecordOrigin` in usage-collector-v2.yaml is `enum: [live, backfill]`.
    // These strings are a wire contract and a bounded metric-label
    // vocabulary at once, so they are asserted against literals rather
    // than against the enum.
    assert_eq!(RecordOrigin::Live.as_str(), "live");
    assert_eq!(RecordOrigin::Backfill.as_str(), "backfill");
}

#[test]
fn record_origin_all_lists_every_variant() {
    // Mirrors `entry_type_all_lists_every_variant`: an exhaustive match
    // with no wildcard breaks this until `ALL` is updated too.
    for variant in RecordOrigin::ALL {
        match variant {
            RecordOrigin::Live | RecordOrigin::Backfill => {}
        }
    }
    assert_eq!(RecordOrigin::ALL.len(), 2, "two variants, both in ALL");
}

#[test]
fn record_origin_wire_labels_is_derived_from_all() {
    assert_eq!(
        RecordOrigin::wire_labels(),
        &["live", "backfill"],
        "wire_labels must list every RecordOrigin::ALL entry's as_str(), in order"
    );
}

#[test]
fn record_origin_serialises_to_its_wire_spelling() {
    assert_eq!(
        serde_json::to_value(RecordOrigin::Live).expect("serializes"),
        serde_json::json!("live"),
    );
    assert_eq!(
        serde_json::to_value(RecordOrigin::Backfill).expect("serializes"),
        serde_json::json!("backfill"),
    );
}

#[test]
fn record_origin_deserialises_from_its_wire_spelling_and_refuses_anything_else() {
    // Both variants, not just one: the read direction is what a consumer
    // decoding a persisted entry depends on, and a value that only
    // round-trips in one direction is the shape a `rename` on a single
    // variant would produce.
    assert_eq!(
        serde_json::from_value::<RecordOrigin>(serde_json::json!("live")).expect("declared value"),
        RecordOrigin::Live,
    );
    assert_eq!(
        serde_json::from_value::<RecordOrigin>(serde_json::json!("backfill"))
            .expect("declared value"),
        RecordOrigin::Backfill,
    );
    // The marker is closed. A third value is a contract violation, not a
    // forward-compatible extension: a consumer that cannot tell imported
    // history from live consumption is the gap the marker exists to close.
    serde_json::from_value::<RecordOrigin>(serde_json::json!("imported"))
        .expect_err("RecordOrigin is closed");
    serde_json::from_value::<RecordOrigin>(serde_json::json!("Live"))
        .expect_err("the wire spelling is lowercase");
}

#[test]
fn the_projection_stamps_the_origin_it_is_handed() {
    let submission = sample_create_usage_record(None, None);
    let live = submission
        .clone()
        .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
        .expect("valid submission");
    let backfilled = submission
        .try_into_usage_record(RecordOrigin::Backfill, SAMPLE_ACCEPTED_AT)
        .expect("valid submission");

    assert_eq!(live.origin, RecordOrigin::Live);
    assert_eq!(backfilled.origin, RecordOrigin::Backfill);
}

#[test]
fn origin_is_not_an_input_to_the_derived_identity() {
    // The dedup identity is the 6-tuple
    // (tenant, gts_type, key, window_start, window_end, entry_type) and
    // `origin` is not one of its six members
    // (`cpt-cf-usage-collector-adr-record-identity-derivation`). This is
    // load-bearing rather than incidental: re-importing history that was
    // once emitted live has to collide with the entry it re-creates so the
    // store can absorb it as a duplicate, and it can only collide if the
    // identifier ignores the path.
    let submission = sample_create_usage_record(None, None);
    let live = submission
        .clone()
        .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
        .expect("valid submission");
    let backfilled = submission
        .try_into_usage_record(RecordOrigin::Backfill, SAMPLE_ACCEPTED_AT)
        .expect("valid submission");

    assert_eq!(live.id, backfilled.id);
}

#[test]
fn a_create_submission_cannot_carry_an_origin() {
    // `origin` is server-assigned, so the ingestion shape has no such
    // property and its `deny_unknown_fields` shadow refuses one. A caller
    // that could name its own path could label imported history as live
    // consumption, which is the distinction the marker exists to make.
    let mut json = serde_json::to_value(sample_create_usage_record(None, None))
        .expect("the submission serializes through its own codec");
    json.as_object_mut()
        .expect("object")
        .insert("origin".to_owned(), json!("live"));

    serde_json::from_value::<CreateUsageRecord>(json)
        .expect_err("origin is server-assigned and must be refused on the create shape");
}

#[test]
fn the_persisted_wire_shape_carries_origin_and_requires_it() {
    let record = sample_create_usage_record(None, None)
        .try_into_usage_record(RecordOrigin::Backfill, SAMPLE_ACCEPTED_AT)
        .expect("valid submission");
    let json = serde_json::to_value(&record).expect("serializes");

    assert_eq!(
        json.get("origin"),
        Some(&json!("backfill")),
        "every persisted entry carries its origin on the wire",
    );

    // Required, not defaulted. An entry decoded from a body with no
    // `origin` has no truthful value to fall back on, and defaulting to
    // `live` would silently relabel imported history as current
    // consumption.
    let mut without = json;
    without.as_object_mut().expect("object").remove("origin");
    serde_json::from_value::<UsageRecord>(without)
        .expect_err("origin is mandatory on the persisted shape");
}

#[test]
fn a_backfill_origin_round_trips_through_both_shadows() {
    // `sample_usage_record` pins `live`, so the general round-trip never
    // exercises the other variant across the two hand-written halves.
    let record = sample_create_usage_record(None, None)
        .try_into_usage_record(RecordOrigin::Backfill, SAMPLE_ACCEPTED_AT)
        .expect("valid submission");
    let json = serde_json::to_value(&record).expect("serializes");

    assert_eq!(
        serde_json::from_value::<UsageRecord>(json).expect("round-trips"),
        record,
    );
}

fn field_of(err: &UsageCollectorError) -> &str {
    match err {
        UsageCollectorError::InvalidArgument { field, .. } => field.as_str(),
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
}

#[test]
fn attribution_components_are_capped_at_256_characters_not_bytes() {
    // U+00E9 is two bytes: 256 of them are 512 bytes, still 256 characters.
    let at_cap = "\u{e9}".repeat(256);
    let over = "\u{e9}".repeat(257);
    ResourceRef::new(at_cap.clone(), at_cap.clone()).expect("256 characters is the ceiling");
    SubjectRef::new(at_cap.clone(), Some(at_cap)).expect("256 characters is the ceiling");

    let err = ResourceRef::new(over.clone(), "t").expect_err("resource_id over the cap");
    assert_eq!(field_of(&err), "resource_ref.resource_id");
    let err = ResourceRef::new("r", over.clone()).expect_err("resource_type over the cap");
    assert_eq!(field_of(&err), "resource_ref.resource_type");
    let err = SubjectRef::new(over.clone(), None::<String>).expect_err("subject_id over the cap");
    assert_eq!(field_of(&err), "subject_ref.subject_id");
    let err = SubjectRef::new("s", Some(over)).expect_err("subject_type over the cap");
    assert_eq!(field_of(&err), "subject_ref.subject_type");
}

#[test]
fn idempotency_key_and_reason_code_count_characters_not_bytes() {
    IdempotencyKey::new("\u{e9}".repeat(256)).expect("256 characters, 512 bytes");
    IdempotencyKey::new("\u{e9}".repeat(257)).expect_err("257 characters");
    ReasonCode::new("\u{e9}".repeat(128)).expect("128 characters, 256 bytes");
    ReasonCode::new("\u{e9}".repeat(129)).expect_err("129 characters");
}

mod caller_supplied_eq {
    use std::collections::BTreeMap;

    use crate::v2::models::{
        CreateUsageRecord, EntryType, IdempotencyKey, Invalidation, MetadataKey, MeterTypeId,
        ReasonCode, RecordOrigin, ResourceRef, SubjectRef, UsageRecord,
    };
    use crate::v2::quantity::UsageQuantity;

    const METER: &str = "gts.cf.core.uc.usage_record.v2~cf.compute._.vcpu_hours.v1~";
    const OTHER_METER: &str = "gts.cf.core.uc.usage_record.v2~cf.compute._.gb_hours.v1~";

    fn at(unix: i64) -> time::OffsetDateTime {
        time::OffsetDateTime::from_unix_timestamp(unix).expect("valid timestamp")
    }

    fn record() -> UsageRecord {
        CreateUsageRecord {
            entry_type: EntryType::Record,
            gts_type_id: MeterTypeId::new(METER).expect("valid meter id"),
            tenant_id: uuid::Uuid::from_u128(0xCA11),
            resource_ref: ResourceRef::new("res-1", "compute.vm").expect("valid resource ref"),
            subject_ref: None,
            metadata: BTreeMap::new(),
            quantity: UsageQuantity::parse("42.5").expect("valid quantity"),
            idempotency_key: IdempotencyKey::new("idem-cse").expect("valid key"),
            invalidation: None,
            window_start: at(1_700_000_000),
            window_end: at(1_700_003_600),
        }
        .try_into_usage_record(RecordOrigin::Live, at(1_700_003_600))
        .expect("valid fixture")
    }

    #[test]
    fn server_assigned_fields_are_not_compared() {
        let a = record();
        let b = UsageRecord {
            id: uuid::Uuid::from_u128(1),
            accepted_at: a.accepted_at + time::Duration::minutes(5),
            origin: RecordOrigin::Backfill,
            ..a.clone()
        };
        assert!(a.caller_supplied_eq(&b));
    }

    #[test]
    fn every_caller_supplied_field_is_compared() {
        let base = record();
        let subject: SubjectRef =
            serde_json::from_value(serde_json::json!({ "subject_id": "sub-1" }))
                .expect("valid subject ref");
        let variants: Vec<(&str, UsageRecord)> = vec![
            (
                "gts_type_id",
                UsageRecord {
                    gts_type_id: MeterTypeId::new(OTHER_METER).expect("valid"),
                    ..base.clone()
                },
            ),
            (
                "tenant_id",
                UsageRecord {
                    tenant_id: uuid::Uuid::from_u128(0xCA12),
                    ..base.clone()
                },
            ),
            (
                "resource_ref",
                UsageRecord {
                    resource_ref: ResourceRef::new("res-2", "compute.vm").expect("valid"),
                    ..base.clone()
                },
            ),
            (
                "subject_ref",
                UsageRecord {
                    subject_ref: Some(subject),
                    ..base.clone()
                },
            ),
            (
                "metadata",
                UsageRecord {
                    metadata: BTreeMap::from([(
                        MetadataKey::new("region").expect("valid"),
                        "eu".to_owned(),
                    )]),
                    ..base.clone()
                },
            ),
            (
                "quantity",
                UsageRecord {
                    quantity: UsageQuantity::parse("42.500").expect("valid"),
                    ..base.clone()
                },
            ),
            (
                "idempotency_key",
                UsageRecord {
                    idempotency_key: IdempotencyKey::new("idem-other").expect("valid"),
                    ..base.clone()
                },
            ),
            (
                "invalidation",
                UsageRecord {
                    invalidation: Some(Invalidation {
                        target: uuid::Uuid::from_u128(9),
                        reason: ReasonCode::new("emitter_defect").expect("valid"),
                    }),
                    ..base.clone()
                },
            ),
            (
                "window_start",
                UsageRecord {
                    window_start: base.window_start - time::Duration::hours(1),
                    ..base.clone()
                },
            ),
            (
                "window_end",
                UsageRecord {
                    window_end: base.window_end + time::Duration::hours(1),
                    ..base.clone()
                },
            ),
        ];
        for (field, other) in variants {
            assert!(
                !base.caller_supplied_eq(&other),
                "`{field}` must be compared"
            );
        }
    }

    #[test]
    fn window_bounds_compare_as_instants() {
        let a = record();
        let offset = time::UtcOffset::from_hms(2, 0, 0).expect("valid offset");
        let b = UsageRecord {
            window_start: a.window_start.to_offset(offset),
            window_end: a.window_end.to_offset(offset),
            ..a.clone()
        };
        assert!(
            a.caller_supplied_eq(&b),
            "one instant under two offsets is one bound"
        );
    }
}

mod explicit_null_idempotency_key {
    use crate::v2::models::CreateUsageRecord;

    fn body(extra: &serde_json::Value) -> serde_json::Value {
        // `entry_type` is required with no default, so every body here
        // declares one; the invalidation cases override it through `extra`.
        let mut body = serde_json::json!({
            "entry_type": "record",
            "gts_type_id": "gts.cf.core.uc.usage_record.v2~cf.compute._.vcpu_hours.v1~",
            "tenant_id": "00000000-0000-4000-8000-000000000001",
            "resource_ref": { "resource_id": "res-1", "resource_type": "compute.vm" },
            "quantity": "1",
            "window_start": "2023-11-14T22:13:20Z",
            "window_end": "2023-11-14T23:13:20Z",
        });
        let object = body.as_object_mut().expect("object");
        for (key, value) in extra.as_object().expect("extra is an object") {
            object.insert(key.clone(), value.clone());
        }
        body
    }

    #[test]
    fn an_explicit_null_key_is_refused_on_a_record() {
        let refused = serde_json::from_value::<CreateUsageRecord>(body(
            &serde_json::json!({ "idempotency_key": null }),
        ));
        assert!(refused.is_err(), "`null` is not a key: {refused:?}");
    }

    #[test]
    fn an_explicit_null_key_is_refused_on_an_invalidation_too() {
        let refused = serde_json::from_value::<CreateUsageRecord>(body(&serde_json::json!({
            "entry_type": "invalidation",
            "idempotency_key": null,
            "reason_code": "emitter_defect",
        })));
        assert!(
            refused.is_err(),
            "the schema types the property `string` on both branches: {refused:?}"
        );
    }

    #[test]
    fn an_absent_key_is_refused_on_both_entry_kinds() {
        // `required` on both `oneOf` branches, and the field is not
        // `Option`, so the codec refuses an omitted key rather than
        // carrying a `None` to a projection to report.
        let mut record = body(&serde_json::json!({}));
        record
            .as_object_mut()
            .expect("object")
            .remove("idempotency_key");
        assert!(
            serde_json::from_value::<CreateUsageRecord>(record).is_err(),
            "an omitted key is a missing required field",
        );

        let mut withdrawal = body(&serde_json::json!({
            "entry_type": "invalidation",
            "reason_code": "emitter_defect",
        }));
        withdrawal
            .as_object_mut()
            .expect("object")
            .remove("idempotency_key");
        assert!(
            serde_json::from_value::<CreateUsageRecord>(withdrawal).is_err(),
            "a withdrawal needs its target's key to resolve the target",
        );
    }

    #[test]
    fn an_invalidation_carries_its_targets_key() {
        // A withdrawal repeats its target's key — which is how the gateway
        // finds the target.
        let decoded = serde_json::from_value::<CreateUsageRecord>(body(&serde_json::json!({
            "entry_type": "invalidation",
            "idempotency_key": "k-1",
            "reason_code": "emitter_defect",
        })))
        .expect("a withdrawal carrying its target's key decodes");
        assert_eq!(decoded.idempotency_key.as_str(), "k-1");
        assert_eq!(
            decoded.entry_type(),
            crate::v2::models::EntryType::Invalidation
        );
    }
}

/// The invariant `project` used to hold with a `debug_assert`, which a
/// release build compiled out — and would then have minted an identifier
/// under the wrong entry type while silently dropping the reason code.
#[test]
fn project_refuses_a_kind_its_caller_failed_to_check() {
    let withdrawal = sample_create_usage_record(None, Some(sample_reason()));
    let err = withdrawal
        .project(RecordOrigin::Live, SAMPLE_ACCEPTED_AT, None)
        .expect_err("an invalidation with no resolved withdrawal");
    assert!(
        matches!(err, UsageCollectorError::Internal { .. }),
        "{err:?}"
    );

    let measurement = sample_create_usage_record(None, None);
    let err = measurement
        .project(
            RecordOrigin::Live,
            SAMPLE_ACCEPTED_AT,
            Some(Invalidation {
                target: target_id(),
                reason: sample_reason(),
            }),
        )
        .expect_err("a record carrying a resolved withdrawal");
    assert!(
        matches!(err, UsageCollectorError::Internal { .. }),
        "{err:?}"
    );
}

/// A NUL in a metadata value is a 400 at ingestion, not a 500 from the plugin.
///
/// Metadata reaches storage as a `jsonb` document and no `jsonb` string can
/// carry a NUL, so an admitted one fails inside the plugin as an `Internal` —
/// a 500 the caller provoked. The key is already refused for this (among
/// wider) reasons; the value has to be too.
#[test]
fn a_nul_in_a_metadata_value_is_refused_at_ingestion() {
    let mut submission = sample_create_usage_record(None, None);
    submission
        .metadata
        .insert(metadata_key("region"), "eu\0west".to_owned());
    let err = submission
        .clone()
        .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
        .expect_err("a NUL in a metadata value must be refused");
    assert_eq!(field_of(&err), "metadata");
    assert!(
        err.to_string().contains("region"),
        "the violation must name the offending key; got {err}"
    );

    // Every other control character stores fine, so none of them is refused.
    submission
        .metadata
        .insert(metadata_key("region"), "eu\nwest\t1".to_owned());
    submission
        .try_into_usage_record(RecordOrigin::Live, SAMPLE_ACCEPTED_AT)
        .expect("only NUL is refused");
}

/// One attribution component reports one `field_violations[].field` spelling,
/// whichever of its three rules fired.
///
/// A client mapping a violation back to its own form otherwise has to know
/// which rule rejected the value to know which path it gets.
#[test]
fn one_attribution_component_reports_one_field_whichever_rule_fires() {
    let over = "x".repeat(257);
    for (expected, err) in [
        (
            "resource_ref.resource_id",
            ResourceRef::new("", "compute.vm").expect_err("empty"),
        ),
        (
            "resource_ref.resource_id",
            ResourceRef::new("vm\0bad", "compute.vm").expect_err("control character"),
        ),
        (
            "resource_ref.resource_id",
            ResourceRef::new(over.as_str(), "compute.vm").expect_err("over the cap"),
        ),
        (
            "subject_ref.subject_type",
            SubjectRef::new("principal-1", Some("")).expect_err("empty"),
        ),
        (
            "subject_ref.subject_type",
            SubjectRef::new("principal-1", Some(over.as_str())).expect_err("over the cap"),
        ),
    ] {
        assert_eq!(field_of(&err), expected);
    }
}
