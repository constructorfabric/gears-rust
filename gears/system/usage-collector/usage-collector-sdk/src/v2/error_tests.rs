//! Unit tests for the [`UsageCollectorError`] constructors whose message text
//! is itself normative, plus the retry-classification and variant-arity pins.
//!
//! Most constructors carry prose no document constrains, and pinning that would
//! freeze wording for its own sake. The ones below are different: DESIGN §3.1's
//! Target resolution row and
//! `cpt-cf-usage-collector-adr-append-only-invalidation`'s Valid target rule
//! both say what an unresolvable target's message has to tell the emitter.
//!
//! Nothing here reads `docs/` from disk. A pin that scrapes a sentence or a
//! markdown table out of a document reds on a reword or a reflow, and a
//! `contains` check over a whole file passes on any superstring — so such a pin
//! fails for the wrong reasons and holds for the wrong ones. Where code and
//! document have to agree, the exhaustive wildcard-free matches below fail the
//! *build* on a new variant; checking that the documents then say the same
//! belongs in a check over `docs/`, not in this crate's unit tests.

use super::*;

/// The target identifier is derived, never sent, so the rejection has to name
/// the inputs it was derived from.
///
/// DESIGN §3.1, Target resolution: nothing found is `NotFound`, "whose message
/// says the target is identified by tenant, GTS type, idempotency key, and
/// covered period, since a typo in any of them surfaces there rather than as a
/// field mismatch". ADR 0010's Valid target rule says the same.
#[test]
fn the_unresolvable_target_rejection_names_the_four_locating_inputs() {
    let target = Uuid::from_u128(0x0BAD_1DEA);
    let err = UsageCollectorError::invalidation_target_not_found(target);
    let UsageCollectorError::NotFound {
        reason,
        name,
        detail,
        ..
    } = &err
    else {
        panic!("expected NotFound, got {err:?}");
    };
    assert_eq!(*reason, NotFoundReason::InvalidationTargetNotFound);
    assert_eq!(name, &target.to_string());

    // Each locating input is asserted separately, so a message that drops one
    // fails on that one rather than on an opaque whole-string comparison.
    for input in ["tenant", "GTS type", "idempotency key", "covered period"] {
        assert!(
            detail.contains(input),
            "the rejection must name `{input}` as part of what identifies the target; got {detail}"
        );
    }

    // The derived identifier stays, for an operator correlating the 404
    // against a log line or against `name`.
    assert!(detail.contains(&target.to_string()), "{detail}");
}

/// The message must not send the emitter after `invalidates`.
///
/// `invalidates` is server-assigned (DESIGN §3.1, Field ownership) and
/// [`crate::v2::CreateUsageRecord`] declares no such property, so an emitter told
/// the rejection is about `invalidates` looks for a field they had no way to
/// send. This is the half a `contains` check on the locating inputs cannot
/// catch: a message can name them all and still lead with the wrong noun.
#[test]
fn the_unresolvable_target_rejection_names_no_field_the_emitter_never_sent() {
    let err = UsageCollectorError::invalidation_target_not_found(Uuid::from_u128(7));
    let UsageCollectorError::NotFound { detail, .. } = &err else {
        panic!("expected NotFound, got {err:?}");
    };
    assert!(
        !detail.contains("invalidates"),
        "`invalidates` is server-assigned and absent from the ingestion shape, so the \
         rejection must not present it as the caller's field; got {detail}"
    );
}

/// A quota rejection is a 429 (`ResourceExhausted`) carrying an integer retry
/// delay and a detail naming both the allowance and the submitted count.
///
/// A detail assertion, not a variant assertion (spec §7 / §15.4 rule 3): an
/// operator reading one envelope must be able to tell a burst from a sustained
/// overrun, so both numbers appear in `detail` rather than only the variant
/// discriminating the outcome.
#[test]
fn a_quota_rejection_is_a_429_carrying_an_integer_retry_delay() {
    let err = UsageCollectorError::ingestion_quota_exceeded(IngestionQuotaExceededArgs {
        allowance: 4000,
        submitted: 5000,
        retry_after_seconds: 1,
    });
    match &err {
        UsageCollectorError::ResourceExhausted {
            retry_after_seconds,
            detail,
        } => {
            assert_eq!(*retry_after_seconds, 1);
            assert!(detail.contains("4000"), "allowance in detail: {detail}");
            assert!(
                detail.contains("5000"),
                "submitted count in detail: {detail}"
            );
        }
        other => panic!("expected ResourceExhausted, got {other:?}"),
    }
}

/// DESIGN §3.3 Error Contract: `retry_after()` is "the complete retry decision"
/// and must answer `Some` for a quota rejection, one of the outcomes it names
/// retryable.
#[test]
fn retry_after_is_some_for_a_quota_rejection() {
    let err = UsageCollectorError::ingestion_quota_exceeded(IngestionQuotaExceededArgs {
        allowance: 4000,
        submitted: 5000,
        retry_after_seconds: 7,
    });
    assert_eq!(err.retry_after(), Some(std::time::Duration::from_secs(7)));
}

/// `ServiceUnavailable` carrying a plugin-supplied hint answers `Some` —
/// another of DESIGN §3.3's retryable outcomes.
#[test]
fn retry_after_is_some_for_service_unavailable_s_hint() {
    let err = UsageCollectorError::service_unavailable("downstream reset", Some(30));
    assert_eq!(err.retry_after(), Some(std::time::Duration::from_secs(30)));
}

/// A bare, config-free `ServiceUnavailable` without a plugin-supplied hint
/// still answers `None`: this crate carries no configuration and cannot
/// synthesize a value it was never given. **Where it is given one, it answers
/// `Some`** — that half is
/// [`retry_after_is_some_for_plugin_unavailable_with_the_configured_default`]
/// below.
#[test]
fn retry_after_is_none_without_a_hint() {
    assert_eq!(
        UsageCollectorError::plugin_unavailable(None).retry_after(),
        None
    );
}

/// The other half: `plugin_unavailable`'s `Option<u64>` parameter reaches
/// `retry_after()` exactly like [`UsageCollectorError::service_unavailable`]'s
/// does — supplying `Some` here is indistinguishable, from this crate's
/// perspective, from a plugin-supplied hint. The host's
/// `domain::error::lift_domain_error` supplies the configured
/// `unavailable_retry_after_secs` default this way, for every dispatch that
/// reaches no plugin.
#[test]
fn retry_after_is_some_for_plugin_unavailable_with_the_configured_default() {
    assert_eq!(
        UsageCollectorError::plugin_unavailable(Some(7)).retry_after(),
        Some(std::time::Duration::from_secs(7))
    );
}

/// A bare, config-free `Conflict(TargetNotConverged)` still answers `None`:
/// this crate carries no configuration of its own. **Where the host supplies the
/// configured `target_not_converged_retry_after_secs` delay, it answers `Some`**
/// — that half is
/// [`retry_after_is_some_for_target_not_converged_with_the_configured_default`]
/// below.
#[test]
fn retry_after_is_none_for_target_not_converged_the_sdk_variant_carries_no_delay() {
    let err = UsageCollectorError::target_not_converged(Uuid::from_u128(1), None);
    assert_eq!(err.retry_after(), None);
}

/// The other half: see
/// [`retry_after_is_none_for_target_not_converged_the_sdk_variant_carries_no_delay`].
/// The host's `domain::error::lift_domain_error` supplies the configured
/// `target_not_converged_retry_after_secs` default this way, for every
/// `DomainError::TargetNotConverged`.
#[test]
fn retry_after_is_some_for_target_not_converged_with_the_configured_default() {
    let err = UsageCollectorError::target_not_converged(Uuid::from_u128(1), Some(3));
    assert_eq!(err.retry_after(), Some(std::time::Duration::from_secs(3)));
}

/// A category DESIGN §3.3 never calls retryable answers `None`.
#[test]
fn retry_after_is_none_for_a_non_retryable_category() {
    assert_eq!(
        UsageCollectorError::permission_denied("denied").retry_after(),
        None,
    );
}

/// `is_infrastructure_retryable()` is true for `ServiceUnavailable` alone, and
/// is not the complete retry decision — `retry_after() -> Option<Duration>` is
/// (DESIGN §3.3).
///
/// The division of labour the name states: the other two outcomes §3.3 calls
/// retryable answer `false` to this predicate and still hand `retry_after` a
/// delay. The method was once spelled `is_retryable` — the name a retry-aware
/// consumer reaches for first — while answering only the narrow question, so
/// two public predicates disagreed with nothing in either name saying which
/// was which.
#[test]
fn the_narrow_retry_predicate_is_true_for_service_unavailable_alone() {
    assert!(
        UsageCollectorError::plugin_unavailable(None).is_infrastructure_retryable(),
        "`ServiceUnavailable` is the one classification this predicate recognizes"
    );

    // The other two outcomes §3.3 calls retryable answer `false` here and
    // still hand `retry_after` a delay — the division of labour the name now
    // states rather than contradicts.
    let quota = UsageCollectorError::ingestion_quota_exceeded(IngestionQuotaExceededArgs {
        allowance: 4000,
        submitted: 5000,
        retry_after_seconds: 7,
    });
    assert!(!quota.is_infrastructure_retryable());
    assert!(
        quota.retry_after().is_some(),
        "a quota rejection is retryable after its delay, which is why this predicate is not \
         the complete decision"
    );

    let not_converged = UsageCollectorError::target_not_converged(Uuid::from_u128(1), Some(3));
    assert!(!not_converged.is_infrastructure_retryable());
    assert!(
        not_converged.retry_after().is_some(),
        "an unconverged target is retryable after its delay, for the same reason"
    );

    assert!(
        !UsageCollectorError::internal("x").is_infrastructure_retryable(),
        "and a classification neither calls retryable answers `false` on both"
    );
    assert_eq!(UsageCollectorError::internal("x").retry_after(), None);
}

// ── The `UsageCollectorError` variant set ─────────────────────────────

/// [`UsageCollectorError`] carries exactly these nine variants, every one of
/// them constructible.
///
/// `variant_name` is the pin's load-bearing half: an exhaustive match with no
/// wildcard arm, over a real [`UsageCollectorError`] value for every variant.
/// `UsageCollectorError` is `#[non_exhaustive]`, but that attribute only forces
/// a wildcard in a *different* crate — this module is a child of `error` itself,
/// so a new variant added anywhere in this crate makes this match fail to
/// compile before the test can run. That is what makes this a pin rather than a
/// snapshot: a tenth variant has to be given an arm here deliberately, and a
/// row in DESIGN §3.3's Error Contract table at the same time.
#[test]
fn the_error_enum_has_exactly_nine_variants_every_one_constructible() {
    /// Exhaustive, wildcard-free: see this test's doc for why a tenth
    /// variant fails the *build*, not just this assertion.
    fn variant_name(e: &UsageCollectorError) -> &'static str {
        match e {
            UsageCollectorError::PermissionDenied { .. } => "PermissionDenied",
            UsageCollectorError::InvalidArgument { .. } => "InvalidArgument",
            UsageCollectorError::CursorRejected { .. } => "CursorRejected",
            UsageCollectorError::NotFound { .. } => "NotFound",
            UsageCollectorError::AlreadyExists { .. } => "AlreadyExists",
            UsageCollectorError::Conflict { .. } => "Conflict",
            UsageCollectorError::ServiceUnavailable { .. } => "ServiceUnavailable",
            UsageCollectorError::ResourceExhausted { .. } => "ResourceExhausted",
            UsageCollectorError::Internal { .. } => "Internal",
        }
    }

    // One concrete value per variant, so `variant_name` actually runs over each
    // rather than existing only to satisfy the compiler. `AlreadyExists` is
    // reserved and has no constructor, so it is built directly — its fields are
    // private but this module is `error`'s own child.
    let one_of_each: [UsageCollectorError; 9] = [
        UsageCollectorError::permission_denied("x"),
        UsageCollectorError::invalid_batch_size(0, 1, 2),
        UsageCollectorError::cursor_query_mismatch(),
        UsageCollectorError::usage_record_not_found(Uuid::nil()),
        UsageCollectorError::AlreadyExists {
            resource_type: crate::v2::USAGE_RECORD_RESOURCE,
            name: "x".to_owned(),
            detail: "x".to_owned(),
        },
        UsageCollectorError::idempotency_conflict(
            &crate::v2::models::IdempotencyKey::new("key").unwrap(),
            Uuid::nil(),
        ),
        UsageCollectorError::plugin_unavailable(None),
        UsageCollectorError::ingestion_quota_exceeded(IngestionQuotaExceededArgs {
            allowance: 1,
            submitted: 1,
            retry_after_seconds: 1,
        }),
        UsageCollectorError::internal("x"),
    ];

    let names: std::collections::BTreeSet<&'static str> =
        one_of_each.iter().map(variant_name).collect();

    assert_eq!(
        names.len(),
        one_of_each.len(),
        "every entry above must build a distinct variant, or one of the nine goes \
         unexercised while the array length still looks right; got {names:?}"
    );
}

/// [`NotFoundReason`]'s arity, pinned exhaustively.
///
/// Unlike `ValidationReason` and `ConflictReason`, this type is "**Not projected
/// onto the wire, and deliberately not**" (its own doc in `reason.rs`): no
/// `SCREAMING_SNAKE` constants, no `from_wire` / `as_wire`, and it never appears
/// on a `Problem` body.
///
/// `NotFoundReason` carries no `Unknown(String)` either — it has no external
/// producer, so there is nothing for an `Unknown` catch-all to preserve, and
/// `reason.rs`'s own doc argues this directly. This test's match has no wildcard
/// and no `Unknown` arm; a new variant fails this *build*, in this crate, for
/// the same reason the variant-set pin above does. The host-side
/// `classify_record_error` match in `usage-collector/src/domain/service.rs`
/// *does* need a wildcard arm, because that match is in a different crate — an
/// asymmetry to pin rather than to report.
#[test]
fn not_found_reason_has_exactly_three_variants_and_no_unknown_catch_all() {
    /// Exhaustive, wildcard-free, in the type's own declaring crate — see
    /// this test's doc.
    fn variant_name(reason: NotFoundReason) -> &'static str {
        match reason {
            NotFoundReason::DeclarationNotFound => "DeclarationNotFound",
            NotFoundReason::UsageRecordNotFound => "UsageRecordNotFound",
            NotFoundReason::InvalidationTargetNotFound => "InvalidationTargetNotFound",
        }
    }

    let names: Vec<&'static str> = [
        NotFoundReason::DeclarationNotFound,
        NotFoundReason::UsageRecordNotFound,
        NotFoundReason::InvalidationTargetNotFound,
    ]
    .into_iter()
    .map(variant_name)
    .collect();

    assert_eq!(
        names,
        [
            "DeclarationNotFound",
            "UsageRecordNotFound",
            "InvalidationTargetNotFound"
        ],
        "NotFoundReason must stay exactly these three variants with no Unknown catch-all; a \
         fourth variant needs an explicit arm above (and, per reason.rs's own doc, a deliberate \
         decision in every host-side matcher) before this test can even compile again"
    );
}

// ── UsageRecordIdInvalid lifts to a caller-facing rejection ─────────────────

/// A derivation refusal is caller-supplied data that did not pass, never an
/// internal fault: every variant has to reach the caller as `InvalidArgument`
/// naming the field they actually sent wrong.
#[test]
fn a_derivation_refusal_names_the_field_it_came_from() {
    let early = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
    let late = OffsetDateTime::from_unix_timestamp(1_700_003_600).unwrap();

    for (invalid, expected_field) in [
        (
            crate::v2::id::UsageRecordIdInvalid::PeriodInverted {
                window_start: late,
                window_end: early,
            },
            WINDOW_END_FIELD,
        ),
        (
            crate::v2::id::UsageRecordIdInvalid::WindowStart(
                crate::v2::id::PeriodBoundInvalid::YearOutOfRange { year: -1 },
            ),
            WINDOW_START_FIELD,
        ),
        (
            crate::v2::id::UsageRecordIdInvalid::WindowEnd(
                crate::v2::id::PeriodBoundInvalid::YearOutOfRange { year: -1 },
            ),
            WINDOW_END_FIELD,
        ),
    ] {
        let err = UsageCollectorError::from(invalid);
        let UsageCollectorError::InvalidArgument { field, reason, .. } = &err else {
            panic!("expected InvalidArgument, got {err:?}");
        };
        assert_eq!(field, expected_field);
        assert_eq!(*reason, ValidationReason::Validation);
    }
}

// ── IdempotencyConflict carries the validated key ───────────────────────────

/// Builds the stored entry an idempotency conflict is raised against. Only
/// the key and the id matter to the assertions below.
fn conflicting_stored_entry() -> crate::v2::stored::StoredUsageRecord {
    use crate::v2::models::{IdempotencyKey, RecordOrigin, ResourceRef};
    use crate::v2::quantity::UsageQuantity;

    let window_start = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
    crate::v2::stored::StoredUsageRecord {
        id: Uuid::from_u128(0x0BAD_1DEA),
        gts_type_uuid: Uuid::from_u128(0x00A1),
        tenant_id: Uuid::from_u128(0x0001),
        resource_ref: ResourceRef::new("vm-1", "compute.vm").unwrap(),
        subject_ref: None,
        metadata: std::collections::BTreeMap::new(),
        quantity: UsageQuantity::parse("42").unwrap(),
        idempotency_key: IdempotencyKey::new("stored-key").unwrap(),
        accepted_at: window_start,
        origin: RecordOrigin::Live,
        invalidation: None,
        window_start,
        window_end: window_start,
    }
}

/// The key reaching the host is the same validated newtype the entry it was
/// read off carries. Dropping to `String` exactly at the reporting boundary
/// made every call site spell `.as_str()` and let an unvalidated key be
/// reported as if it had passed.
#[test]
fn an_idempotency_conflict_reports_the_validated_key() {
    use crate::v2::models::IdempotencyKey;

    let dispatched = IdempotencyKey::new("dispatched-key").unwrap();
    let err = UsageCollectorPluginError::idempotency_conflict(
        dispatched.clone(),
        conflicting_stored_entry(),
    );

    let UsageCollectorPluginError::IdempotencyConflict {
        idempotency_key,
        existing,
    } = &err
    else {
        panic!("expected IdempotencyConflict, got {err:?}");
    };
    assert_eq!(*idempotency_key, dispatched);
    assert_eq!(
        existing.idempotency_key,
        IdempotencyKey::new("stored-key").unwrap(),
        "the stored entry keeps its own key; the two are not interchangeable",
    );
}

// ── Result budget ──────────────────────────────────────────────────────────

/// `UsageCollectorError` is returned from fallible surfaces across three
/// crates, and clippy's `result_large_err` fires at 128 bytes. Keeping the
/// envelope inside that budget is what lets those surfaces drop the
/// `#[allow(clippy::result_large_err)]` they each carried, whose own reason
/// admitted that out-of-crate callers inherited the lint.
#[test]
fn the_error_envelope_fits_the_result_budget() {
    // Strictly under: `result_large_err` fires at `>=` the threshold, so an
    // error of exactly 128 bytes already reds every surface whose
    // `#[allow(clippy::result_large_err)]` this crate and its consumers
    // dropped once the envelope fit.
    assert!(
        std::mem::size_of::<UsageCollectorError>() < 128,
        "UsageCollectorError is {} bytes; clippy::result_large_err fires at the 128-byte \
         large-error-threshold in clippy.toml",
        std::mem::size_of::<UsageCollectorError>(),
    );
}

/// `ConflictOutcome::reason()` reaches the wire as `context.reason` and drives
/// `Conflict`'s `Display`, so two swapped arms misreport a 409 silently.
/// `AlreadyInvalidated` is built through its own constructor, which nothing
/// else in this file exercises.
#[test]
fn every_conflict_outcome_yields_its_wire_reason() {
    let already = UsageCollectorError::already_invalidated(AlreadyInvalidatedArgs {
        target: Uuid::from_u128(0x07A4_66E7),
        invalidated_by: Uuid::from_u128(0xB1),
        reason_code: crate::v2::models::ReasonCode::new("emitter_defect")
            .expect("valid reason code"),
    });
    let UsageCollectorError::Conflict {
        outcome: already, ..
    } = already
    else {
        panic!("already_invalidated must build a Conflict");
    };

    for (outcome, expected) in [
        (
            ConflictOutcome::IdempotencyConflict,
            ConflictReason::IdempotencyConflict,
        ),
        (already, ConflictReason::AlreadyInvalidated),
        (
            ConflictOutcome::TargetNotConverged {
                retry_after_seconds: Some(5),
            },
            ConflictReason::TargetNotConverged,
        ),
    ] {
        assert_eq!(outcome.reason(), expected, "for {outcome:?}");
    }
}

/// A rejected value reaches both the caller's `detail` and the log line, and
/// the two branches that echo one have run no length or control-character
/// check on it yet.
#[test]
fn an_echoed_rejected_value_is_bounded_and_escaped() {
    let huge = "9".repeat(10_000);
    let rendered = echo_rejected_value(&huge);
    assert!(
        rendered.len() < MAX_ECHOED_VALUE_CHARS * 2,
        "a 10k value must not echo whole: {} characters",
        rendered.len(),
    );
    assert!(rendered.contains("10000 characters in all"), "{rendered}");

    let injected = "line\nsecond\rthird";
    let rendered = echo_rejected_value(injected);
    assert_eq!(rendered, "line\\nsecond\\rthird");

    // A value inside the bound is echoed verbatim.
    assert_eq!(echo_rejected_value("gts.x~y~"), "gts.x~y~");
}

/// The length branch of `MeterTypeId::new` rejects a value nothing has
/// bounded, so the detail it builds must not carry it whole.
#[test]
fn an_oversized_meter_type_id_is_not_echoed_whole() {
    let huge = format!("gts.cf.core.uc.usage_record.v2~{}~", "x".repeat(100_000));
    let err = MeterTypeId::new(huge.clone()).expect_err("past the 512-byte bound");
    let UsageCollectorError::InvalidArgument { detail, .. } = err else {
        panic!("expected InvalidArgument");
    };
    assert!(
        detail.len() < 512,
        "a {}-byte gts_type_id produced a {}-byte detail",
        huge.len(),
        detail.len(),
    );
}

/// The two cursor-bearing query parameters, pinned against their literals the
/// way every other wire-spelling map in this module is.
#[test]
fn cursor_field_wire_spellings_are_the_query_parameter_names() {
    assert_eq!(CursorField::Cursor.as_wire(), "cursor");
    assert_eq!(CursorField::Until.as_wire(), "until");
}

/// Which parameter each cursor rejection is stamped against.
///
/// `until` is the feed's replay bound and `cursor` the resume point, so a
/// defect in one reported against the other tells a caller to fix the
/// parameter it did not send. Only the two `until` constructors carry
/// [`CursorField::Until`]; an arm swapped either way fails here.
#[test]
fn each_cursor_rejection_names_the_parameter_it_is_about() {
    fn field_of(err: &UsageCollectorError) -> CursorField {
        match err {
            UsageCollectorError::CursorRejected { field, .. } => *field,
            other => panic!("expected CursorRejected, got {other:?}"),
        }
    }

    for (err, expected) in [
        (
            UsageCollectorError::inadmissible_until_keyset("defect"),
            CursorField::Until,
        ),
        (
            UsageCollectorError::until_query_mismatch(),
            CursorField::Until,
        ),
        (
            UsageCollectorError::inadmissible_cursor_keyset("defect"),
            CursorField::Cursor,
        ),
        (
            UsageCollectorError::inadmissible_feed_cursor("defect"),
            CursorField::Cursor,
        ),
        (
            UsageCollectorError::cursor_query_mismatch(),
            CursorField::Cursor,
        ),
        (
            UsageCollectorError::cursor_subscription_mismatch(),
            CursorField::Cursor,
        ),
        (
            UsageCollectorError::unsupported_cursor_direction("defect"),
            CursorField::Cursor,
        ),
    ] {
        assert_eq!(field_of(&err), expected, "wrong parameter for {err}");
    }
}

/// A caller value echoed into a `detail` is escaped, so a control character
/// reaches neither the operator log nor the 400 body verbatim.
///
/// `$filter` string literals are `[^']` and an `$orderby` key is a raw
/// whitespace-delimited token, so both arrive unsanitized; the fold string
/// arrives from a resolved type declaration this crate has checked in no way.
#[test]
fn caller_and_registry_values_are_escaped_before_they_reach_a_detail() {
    let raw = "a\u{1b}[31mb\nc";
    let escaped = "a\\u{1b}[31mb\\nc";

    for err in [
        UsageCollectorError::inadmissible_filter_literal("entry_type", raw, &["record"]),
        UsageCollectorError::inadmissible_order_key(raw),
        UsageCollectorError::duplicate_order_key(raw),
        UsageCollectorError::mixed_direction_order(raw),
        UsageCollectorError::invalid_aggregation_fold(raw),
    ] {
        let detail = match &err {
            UsageCollectorError::InvalidArgument { detail, .. } => detail.clone(),
            other => panic!("expected InvalidArgument, got {other:?}"),
        };
        assert!(
            detail.contains(escaped),
            "detail must carry the escaped rendering; got {detail}"
        );
        assert!(
            !detail.contains('\u{1b}') && !detail.contains('\n'),
            "detail must carry no raw control character; got {detail:?}"
        );
    }
}

/// A PDP reason is readable only through the accessor: `Display` renders none,
/// so an error written out generically cannot carry it onto the wire.
#[test]
fn a_pdp_reason_is_reachable_only_through_the_accessor() {
    let err = UsageCollectorError::permission_denied("subject lacks usage_record.create on tenant");
    assert_eq!(
        err.pdp_detail(),
        Some("subject lacks usage_record.create on tenant")
    );
    assert_eq!(err.to_string(), "authorization denied");
    assert_eq!(UsageCollectorError::internal("x").pdp_detail(), None);
}
