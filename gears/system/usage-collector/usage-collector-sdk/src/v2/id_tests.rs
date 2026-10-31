use time::{Date, Month, OffsetDateTime, Time, UtcOffset};
use toolkit_gts::gts_id;
use uuid::Uuid;

use crate::v2::id::{
    PeriodBoundInvalid, USAGE_RECORD_ID_NAMESPACE, UsageRecordIdInvalid, canonical_period_bound,
    derive_usage_record_id,
};
use crate::v2::models::{EntryType, IdempotencyKey, MeterTypeId};

fn tenant() -> Uuid {
    Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()
}
fn gts() -> MeterTypeId {
    // Must be a valid derived GTS type id: `~`-terminated, adding exactly one
    // segment to the reserved usage-record base.
    MeterTypeId::new(gts_id!(
        "cf.core.uc.usage_record.v2~cf.mini_chat._.tokens_consumed.v1~"
    ))
    .unwrap()
}
fn key(s: &str) -> IdempotencyKey {
    IdempotencyKey::new(s).unwrap()
}
/// `2023-11-14T22:13:20.000000Z`
fn ws() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap()
}
/// `2023-11-14T23:13:20.000000Z`
fn we() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_003_600).unwrap()
}
fn expect(raw: &str) -> Uuid {
    Uuid::parse_str(raw).unwrap()
}

#[test]
fn derive_is_deterministic() {
    assert_eq!(
        derive_usage_record_id(
            tenant(),
            &gts(),
            &key("idem-1"),
            ws(),
            we(),
            EntryType::Record
        )
        .unwrap(),
        derive_usage_record_id(
            tenant(),
            &gts(),
            &key("idem-1"),
            ws(),
            we(),
            EntryType::Record
        )
        .unwrap(),
    );
}

#[test]
fn derive_matches_golden_vector() {
    // UUIDv5(NS, "11111111-1111-1111-1111-111111111111" 0x1F
    //            "gts.cf.core.uc.usage_record.v2~cf.mini_chat._.tokens_consumed.v1~" 0x1F
    //            "idem-1" 0x1F
    //            "2023-11-14T22:13:20.000000Z" 0x1F
    //            "2023-11-14T23:13:20.000000Z" 0x1F
    //            "record")
    //
    // Computed independently of this crate (RFC 4122 UUIDv5 over the
    // pre-image `cpt-cf-usage-collector-adr-record-identity-derivation`
    // fixes). DO NOT hand-edit: regenerate both sides and reconcile.
    assert_eq!(
        derive_usage_record_id(
            tenant(),
            &gts(),
            &key("idem-1"),
            ws(),
            we(),
            EntryType::Record
        )
        .unwrap(),
        expect("b39a59b1-de07-5992-8433-cf418fab3c4b"),
    );
}

#[test]
fn derive_matches_golden_vector_for_an_invalidation() {
    // The same five inputs as `derive_matches_golden_vector`, with the
    // sixth reading `invalidation`:
    //
    // UUIDv5(NS, "11111111-1111-1111-1111-111111111111" 0x1F
    //            "gts.cf.core.uc.usage_record.v2~cf.mini_chat._.tokens_consumed.v1~" 0x1F
    //            "idem-1" 0x1F
    //            "2023-11-14T22:13:20.000000Z" 0x1F
    //            "2023-11-14T23:13:20.000000Z" 0x1F
    //            "invalidation")
    //
    // Computed independently of this crate, like its `record` twin. Both
    // literals are needed: one vector alone pins the pre-image for one
    // entry type and would survive a derivation that ignored the sixth
    // input on the other. DO NOT hand-edit.
    assert_eq!(
        derive_usage_record_id(
            tenant(),
            &gts(),
            &key("idem-1"),
            ws(),
            we(),
            EntryType::Invalidation
        )
        .unwrap(),
        expect("d1ae93eb-8d16-5f88-b2e6-23939ee86869"),
    );
}

/// The pre-image is the six inputs in order, `0x1F`-joined, entry type last.
///
/// `cpt-cf-usage-collector-adr-record-identity-derivation` fixes the order
/// in its formula and the bytes in "The canonical pre-image". This test
/// rebuilds that byte string by hand and digests it here, so the order, the
/// separator and each input's canonical rendering are pinned against the
/// document rather than against the concatenation in `id.rs` — which is the
/// thing under test. `derive_matches_golden_vector` above pins the same
/// pre-image from the other side, as a literal computed outside this crate.
#[test]
fn the_pre_image_is_the_six_inputs_entry_type_last() {
    let pre_image = |entry_type: &[u8]| {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"11111111-1111-1111-1111-111111111111");
        bytes.push(0x1F);
        bytes.extend_from_slice(
            b"gts.cf.core.uc.usage_record.v2~cf.mini_chat._.tokens_consumed.v1~",
        );
        bytes.push(0x1F);
        bytes.extend_from_slice(b"idem-1");
        bytes.push(0x1F);
        bytes.extend_from_slice(b"2023-11-14T22:13:20.000000Z");
        bytes.push(0x1F);
        bytes.extend_from_slice(b"2023-11-14T23:13:20.000000Z");
        bytes.push(0x1F);
        bytes.extend_from_slice(entry_type);
        bytes
    };

    assert_eq!(
        derive_usage_record_id(
            tenant(),
            &gts(),
            &key("idem-1"),
            ws(),
            we(),
            EntryType::Record
        )
        .unwrap(),
        Uuid::new_v5(&USAGE_RECORD_ID_NAMESPACE, &pre_image(b"record")),
        "a measurement's pre-image ends in the literal `record`",
    );
    assert_eq!(
        derive_usage_record_id(
            tenant(),
            &gts(),
            &key("idem-1"),
            ws(),
            we(),
            EntryType::Invalidation
        )
        .unwrap(),
        Uuid::new_v5(&USAGE_RECORD_ID_NAMESPACE, &pre_image(b"invalidation")),
        "a withdrawal's pre-image ends in the literal `invalidation`",
    );
}

#[test]
fn derive_produces_a_v5_uuid() {
    assert_eq!(
        derive_usage_record_id(
            tenant(),
            &gts(),
            &key("idem-1"),
            ws(),
            we(),
            EntryType::Record
        )
        .unwrap()
        .get_version_num(),
        5,
    );
}

#[test]
fn namespace_is_pinned() {
    // Fixed forever: a change re-maps every identifier the gear has issued.
    assert_eq!(
        USAGE_RECORD_ID_NAMESPACE,
        expect("56313026-863b-4de8-b32b-1f96b67306ed"),
    );
}

#[test]
fn distinct_keys_yield_distinct_ids() {
    assert_eq!(
        derive_usage_record_id(
            tenant(),
            &gts(),
            &key("idem-2"),
            ws(),
            we(),
            EntryType::Record
        )
        .unwrap(),
        expect("aa1fb471-5341-5534-8c47-25e6a53b1904"),
    );
}

#[test]
fn distinct_tenants_yield_distinct_ids() {
    let other = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
    assert_eq!(
        derive_usage_record_id(other, &gts(), &key("idem-1"), ws(), we(), EntryType::Record)
            .unwrap(),
        expect("a036baaf-ef53-531a-8b2b-0aa4363698c7"),
    );
}

#[test]
fn distinct_gts_ids_yield_distinct_ids() {
    let other = MeterTypeId::new(gts_id!(
        "cf.core.uc.usage_record.v2~cf.mini_chat._.messages_sent.v1~"
    ))
    .unwrap();
    assert_eq!(
        derive_usage_record_id(
            tenant(),
            &other,
            &key("idem-1"),
            ws(),
            we(),
            EntryType::Record
        )
        .unwrap(),
        expect("93b7cf0b-99ad-506b-bbac-ac8b7cd7cded"),
    );
}

#[test]
fn the_tenant_enters_in_its_lowercase_hyphenated_form() {
    // The base golden vector's tenant is all decimal digits, so it cannot
    // tell a lowercase rendering apart from an uppercase one. This vector
    // carries hex letters, and it is the only thing standing between the
    // pre-image and a `to_string().to_uppercase()` slip. The second
    // assertion is the caller-facing half of the same rule: `Uuid` parses
    // either spelling, and both must reach one pre-image.
    let lower = Uuid::parse_str("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee").unwrap();
    let upper = Uuid::parse_str("AAAAAAAA-BBBB-4CCC-8DDD-EEEEEEEEEEEE").unwrap();
    assert_eq!(
        derive_usage_record_id(lower, &gts(), &key("idem-1"), ws(), we(), EntryType::Record)
            .unwrap(),
        expect("44bf0794-a0cc-5eba-8637-df69c71a893a"),
    );
    assert_eq!(
        derive_usage_record_id(upper, &gts(), &key("idem-1"), ws(), we(), EntryType::Record)
            .unwrap(),
        expect("44bf0794-a0cc-5eba-8637-df69c71a893a"),
    );
}

#[test]
fn a_different_covered_period_yields_a_different_id() {
    // The point of carrying both bounds: one stable per-meter idempotency
    // key covers many periods, and two periods are two entries rather than
    // a collision.
    let one_micro_later = ws() + time::Duration::microseconds(1);
    assert_eq!(
        derive_usage_record_id(
            tenant(),
            &gts(),
            &key("idem-1"),
            one_micro_later,
            we(),
            EntryType::Record
        )
        .unwrap(),
        expect("0f36a080-6119-5450-a480-95278a14278f"),
    );
    assert_ne!(
        derive_usage_record_id(
            tenant(),
            &gts(),
            &key("idem-1"),
            ws(),
            we(),
            EntryType::Record
        )
        .unwrap(),
        derive_usage_record_id(
            tenant(),
            &gts(),
            &key("idem-1"),
            ws(),
            we() + time::Duration::microseconds(1),
            EntryType::Record
        )
        .unwrap(),
        "window_end is an input too",
    );
}

/// Reported rather than panicked: an emitter reproducing an identity offline
/// gets a value it can match on, and the gateway stops repeating the check
/// ahead of every call.
#[test]
fn derive_refuses_reversed_bounds() {
    assert_eq!(
        derive_usage_record_id(
            tenant(),
            &gts(),
            &key("idem-1"),
            we(),
            ws(),
            EntryType::Record,
        ),
        Err(UsageRecordIdInvalid::PeriodInverted {
            window_start: we(),
            window_end: ws(),
        }),
    );
}

#[test]
fn a_point_event_derives_over_equal_bounds() {
    // A confirmation of
    // `cpt-cf-usage-collector-adr-record-identity-derivation`: a point event
    // is a zero-length period, and the derivation needs no separate case for
    // it.
    assert_eq!(
        derive_usage_record_id(
            tenant(),
            &gts(),
            &key("idem-1"),
            ws(),
            ws(),
            EntryType::Record
        )
        .unwrap(),
        expect("22a8df66-1c53-584b-9d77-6085588a37e5"),
    );
}

#[test]
fn equivalent_spellings_of_one_instant_derive_one_id() {
    // A confirmation of
    // `cpt-cf-usage-collector-adr-record-identity-derivation`: the canonical
    // form collapses a non-UTC offset and a fraction of zero / three / six
    // digits onto one pre-image. The
    // bounds are parsed from the wire spellings an emitter would actually
    // send, rather than built from components, because that is the path the
    // equivalence has to hold across. Every pairing must derive the base
    // vector. (UUID case is pinned separately by
    // `the_tenant_enters_in_its_lowercase_hyphenated_form`, which needs a
    // letter-bearing tenant the base vector does not have.)
    let base = expect("b39a59b1-de07-5992-8433-cf418fab3c4b");
    let parse = |raw: &str| {
        OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339).unwrap()
    };
    let derive_over = |start: &str, end: &str| {
        derive_usage_record_id(
            tenant(),
            &gts(),
            &key("idem-1"),
            parse(start),
            parse(end),
            EntryType::Record,
        )
        .unwrap()
    };

    assert_eq!(
        derive_over("2023-11-14T22:13:20Z", "2023-11-14T23:13:20Z"),
        base,
        "an absent fraction pads to six digits",
    );
    assert_eq!(
        derive_over("2023-11-14T22:13:20.000Z", "2023-11-14T23:13:20.000Z"),
        base,
        "a three-digit fraction pads to six digits",
    );
    assert_eq!(
        derive_over("2023-11-14T22:13:20.000000Z", "2023-11-14T23:13:20.000000Z"),
        base,
        "a six-digit fraction is already canonical",
    );
    assert_eq!(
        derive_over("2023-11-15T03:43:20+05:30", "2023-11-15T04:43:20+05:30"),
        base,
        "a non-UTC offset normalizes before the bound is formatted",
    );
    assert_eq!(
        derive_over("2023-11-14T17:13:20-05:00", "2023-11-14T18:13:20-05:00"),
        base,
        "a negative offset normalizes the same way",
    );
}

// ── canonical_period_bound: the 27-character form ───────────────────────────

#[test]
fn canonical_period_bound_is_twenty_seven_characters() {
    let rendered = canonical_period_bound(ws()).unwrap();
    assert_eq!(rendered, "2023-11-14T22:13:20.000000Z");
    assert_eq!(rendered.len(), 27);
}

#[test]
fn canonical_period_bound_always_carries_six_fraction_digits() {
    let with_micros = ws() + time::Duration::microseconds(1);
    assert_eq!(
        canonical_period_bound(with_micros).unwrap(),
        "2023-11-14T22:13:20.000001Z"
    );
    assert_eq!(
        canonical_period_bound(ws() + time::Duration::milliseconds(500)).unwrap(),
        "2023-11-14T22:13:20.500000Z"
    );
}

#[test]
fn canonical_period_bound_zero_pads_every_component() {
    // The base vector's instant has no single-digit component and a
    // four-digit year, so it cannot tell `{:02}` from `{}` on the month,
    // day, hour, minute or second, nor `{:04}` from `{}` on the year. This
    // instant has a single-digit value in every one of those positions —
    // dropping any pad shortens the form below 27 characters and re-maps
    // every identifier derived over it.
    let padded = OffsetDateTime::new_utc(
        Date::from_calendar_date(999, Month::January, 2).unwrap(),
        Time::from_hms_micro(3, 4, 5, 6).unwrap(),
    );
    let rendered = canonical_period_bound(padded).unwrap();
    assert_eq!(rendered, "0999-01-02T03:04:05.000006Z");
    assert_eq!(rendered.len(), 27);
}

#[test]
fn canonical_period_bound_normalizes_a_non_utc_offset() {
    let plus_one = UtcOffset::from_hms(1, 0, 0).unwrap();
    assert_eq!(
        canonical_period_bound(ws().to_offset(plus_one)).unwrap(),
        canonical_period_bound(ws()).unwrap(),
    );
}

/// Refused rather than truncated. Truncating answered one canonical bound
/// for two instants a nanosecond apart, so an external emitter reproducing
/// an identity offline got a plausible wrong UUID for input every ingestion
/// path rejects.
#[test]
fn canonical_period_bound_refuses_a_sub_microsecond_bound() {
    for nanosecond in [1, 1_500, 999_999_999] {
        let bound = ws().replace_nanosecond(nanosecond).unwrap();
        assert_eq!(
            canonical_period_bound(bound),
            Err(PeriodBoundInvalid::SubMicrosecond { nanosecond }),
        );
    }
    // A whole number of microseconds still renders.
    assert_eq!(
        canonical_period_bound(ws().replace_nanosecond(1_000).unwrap()).unwrap(),
        "2023-11-14T22:13:20.000001Z"
    );
}

/// The refusal travels as the bound it was reported on, so a caller is told
/// which of the two it sent wrong.
#[test]
fn deriving_over_a_sub_microsecond_bound_names_the_bound() {
    let fine = we().replace_nanosecond(1).unwrap();
    let err = derive_usage_record_id(
        tenant(),
        &gts(),
        &key("idem-1"),
        ws(),
        fine,
        EntryType::Record,
    )
    .expect_err("a sub-microsecond end has no canonical rendering");
    assert_eq!(
        err,
        UsageRecordIdInvalid::WindowEnd(PeriodBoundInvalid::SubMicrosecond { nanosecond: 1 }),
    );
}

/// Refused rather than rendered: a release build used to answer a string
/// like `-001-...`, still 27 characters and so indistinguishable downstream
/// from a canonical bound, while a debug build panicked.
///
/// Only years below zero: `time` is built without `large-dates`, so `Date`
/// holds `-9999..=9999` and a higher year cannot be constructed at all.
#[test]
fn canonical_period_bound_refuses_a_year_the_fixed_width_cannot_hold() {
    for year in [-1, -9999] {
        let bound = OffsetDateTime::new_utc(
            Date::from_calendar_date(year, Month::January, 1).unwrap(),
            Time::MIDNIGHT,
        );
        assert_eq!(
            canonical_period_bound(bound),
            Err(PeriodBoundInvalid::YearOutOfRange { year }),
            "year {year} is outside the form the derivation publishes"
        );
    }
}

/// Both ends render, so the rejection above is the range and not an
/// off-by-one in the check.
#[test]
fn canonical_period_bound_renders_both_ends_of_the_year_range() {
    for (year, expected) in [
        (0, "0000-01-01T00:00:00.000000Z"),
        (9999, "9999-01-01T00:00:00.000000Z"),
    ] {
        let bound = OffsetDateTime::new_utc(
            Date::from_calendar_date(year, Month::January, 1).unwrap(),
            Time::MIDNIGHT,
        );
        let rendered = canonical_period_bound(bound).unwrap();
        assert_eq!(rendered, expected);
        assert_eq!(rendered.len(), 27);
    }
}

/// `derive_usage_record_id` carries the bound rejection outward rather than
/// panicking, so the year range is one refusal a caller can handle rather
/// than two failure modes that differ by build profile.
#[test]
fn derive_refuses_a_bound_outside_the_year_range() {
    let out_of_range = OffsetDateTime::new_utc(
        Date::from_calendar_date(-1, Month::January, 1).unwrap(),
        Time::MIDNIGHT,
    );
    assert_eq!(
        derive_usage_record_id(
            tenant(),
            &gts(),
            &key("idem-1"),
            out_of_range,
            out_of_range,
            EntryType::Record,
        ),
        Err(UsageRecordIdInvalid::WindowStart(
            PeriodBoundInvalid::YearOutOfRange { year: -1 }
        )),
    );
}

/// The two bound arms name the bound that failed, so a caller can report the
/// refusal against the right field instead of guessing.
#[test]
fn derive_names_the_bound_that_failed() {
    let out_of_range = OffsetDateTime::new_utc(
        Date::from_calendar_date(-1, Month::January, 1).unwrap(),
        Time::MIDNIGHT,
    );
    assert_eq!(
        derive_usage_record_id(
            tenant(),
            &gts(),
            &key("idem-1"),
            ws(),
            out_of_range,
            EntryType::Record,
        ),
        Err(UsageRecordIdInvalid::WindowEnd(
            PeriodBoundInvalid::YearOutOfRange { year: -1 }
        )),
        "a start that renders and an end that does not names window_end",
    );
}

/// Both bounds are judged before their order, so a call that is wrong in both
/// ways names the bound rather than the ordering: a bound with no canonical
/// rendering is the more specific fault.
#[test]
fn derive_reports_an_out_of_range_bound_ahead_of_the_inversion() {
    let low = OffsetDateTime::new_utc(
        Date::from_calendar_date(-1, Month::January, 1).unwrap(),
        Time::MIDNIGHT,
    );
    let high = OffsetDateTime::new_utc(
        Date::from_calendar_date(-1, Month::January, 2).unwrap(),
        Time::MIDNIGHT,
    );
    assert_eq!(
        derive_usage_record_id(
            tenant(),
            &gts(),
            &key("idem-1"),
            high,
            low,
            EntryType::Record,
        ),
        Err(UsageRecordIdInvalid::WindowStart(
            PeriodBoundInvalid::YearOutOfRange { year: -1 }
        )),
    );
}

// ── Entry type, the sixth input ─────────────────────────────────────────────

/// A record and its invalidation differ in exactly one input and derive two
/// identifiers.
///
/// `cpt-cf-usage-collector-adr-record-identity-derivation`, "Decision
/// Outcome": an invalidation "copies the tenant, type, idempotency key and
/// covered period of its target. Its identifier therefore differs from its
/// target's for one reason only, the entry type."
#[test]
fn a_record_and_its_invalidation_derive_two_ids_from_one_key() {
    let record = derive_usage_record_id(
        tenant(),
        &gts(),
        &key("idem-1"),
        ws(),
        we(),
        EntryType::Record,
    )
    .unwrap();
    let invalidation = derive_usage_record_id(
        tenant(),
        &gts(),
        &key("idem-1"),
        ws(),
        we(),
        EntryType::Invalidation,
    )
    .unwrap();

    assert_ne!(
        record, invalidation,
        "the entry type is the sixth input and the only one these two differ in, so a \
         derivation that reads it gives two identifiers. Equal ids mean the entry type is not \
         reaching the pre-image"
    );
}
