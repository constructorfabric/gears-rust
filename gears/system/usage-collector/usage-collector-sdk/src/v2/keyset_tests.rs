use toolkit_odata::SortDir;

use super::{Keyset, KeysetInvalid, MAX_KEYSET_BYTES, MAX_PAGE_SIZE, RecordPage};

/// The gear's REST `$top` cap and both storage plugins hold their own copy
/// of this number until they switch to it, and a value that disagrees either
/// admits a page a plugin will clamp or refuses one the gateway allowed.
/// Changing it here changes all of them.
#[test]
fn the_published_page_size_bound_is_one_thousand() {
    assert_eq!(MAX_PAGE_SIZE, 1000);
}

#[test]
fn a_keyset_carries_its_values_in_order_and_its_direction() {
    let ks = Keyset::new(["2026-01-01T00:00:00Z", "a-uuid"], SortDir::Desc)
        .expect("two values is a well-formed keyset");
    assert_eq!(ks.values(), ["2026-01-01T00:00:00Z", "a-uuid"]);
    assert_eq!(ks.direction(), SortDir::Desc);
}

#[test]
fn an_empty_keyset_is_refused_because_it_is_a_plugin_that_did_not_fill_it() {
    // Mirrors `FeedPositionInvalid::Empty`'s reasoning: zero values is a
    // plugin that did not populate the keyset, not a keyset needing none.
    // Every admissible order is non-empty (the gateway floors it), so an
    // empty keyset cannot be a legitimate answer.
    let err = Keyset::new(Vec::<String>::new(), SortDir::Asc)
        .expect_err("an empty keyset is a plugin defect, not a page");
    assert_eq!(err, KeysetInvalid::Empty);
}

#[test]
fn an_oversized_keyset_is_refused_where_the_plugin_can_still_act_on_it() {
    // `resource_id` and `resource_type` are caller-supplied and both are
    // admissible order keys, so the keyset is the one input to the wire
    // cursor a caller reaches at all — bounded by `cap_attribution`, which
    // this test forges past, so what it exercises is a plugin defect. Refused
    // here rather than at encoding time, for the reason
    // `FeedPosition::new`'s doc gives: in the gateway the only remaining
    // move is to fail a consumer's read.
    //
    // The budget measures the JSON-encoded array `Keyset::new` embeds at
    // `CursorV1::k`, not the raw string. A
    // one-element array of a plain-ASCII value that needs no escaping —
    // `["xxx...x"]` — costs 4 bytes of framing (two brackets, one quote
    // pair) beyond the value's own length, so a value of
    // `MAX_KEYSET_BYTES - 3` bytes encodes to exactly `MAX_KEYSET_BYTES + 1`
    // — one byte over the bound.
    let huge = "x".repeat(MAX_KEYSET_BYTES - 3);
    let err = Keyset::new([huge], SortDir::Asc)
        .expect_err("a keyset past the bound cannot be handed to a consumer");
    assert_eq!(
        err,
        KeysetInvalid::TooLarge {
            actual: MAX_KEYSET_BYTES + 1
        }
    );
}

#[test]
fn the_bound_is_measured_over_every_value_not_over_the_longest() {
    // Two values each inside the bound can exceed it together, and it is
    // the JSON-encoded array — not either value alone — that rides in the
    // cursor. A two-element array of plain-ASCII values needing no escaping
    // — `["xxx...x","xxx...x"]` — costs 7 bytes of framing (two brackets,
    // two quote pairs, one comma) beyond the two values' own lengths, so two
    // values whose lengths sum to `MAX_KEYSET_BYTES - 6` are individually
    // admissible (each one's own one-element array costs only 4 bytes of
    // framing, well under the bound) but together land one byte over it.
    //
    // Split with `div_ceil` / subtraction rather than one halving division:
    // `MAX_KEYSET_BYTES` need not be even (it is not — 2243), and a
    // plain `/ 2` under integer division would floor the sum by one byte and
    // land exactly *on* the bound instead of one over it, turning the
    // `expect_err` below into a false pass. The two lengths differing by at
    // most one still sums to exactly `MAX_KEYSET_BYTES - 6` for any parity.
    let total = MAX_KEYSET_BYTES - 6;
    let first_len = total.div_ceil(2);
    let second_len = total - first_len;
    let first = "x".repeat(first_len);
    let second = "x".repeat(second_len);
    let err =
        Keyset::new([first, second], SortDir::Asc).expect_err("the sum is what the cursor carries");
    assert_eq!(
        err,
        KeysetInvalid::TooLarge {
            actual: MAX_KEYSET_BYTES + 1
        }
    );
}

#[test]
fn a_keyset_at_the_json_encoded_byte_bound_is_accepted() {
    // Mirrors `feed_tests::a_position_at_the_byte_bound_is_accepted`: the
    // bound is inclusive, and nothing else pins that a value landing
    // exactly on it is admitted rather than refused by an off-by-one `>=`.
    // One-element array framing for a plain-ASCII value costs 4 bytes (see
    // the oversized test above), so a value of `MAX_KEYSET_BYTES - 4` bytes
    // encodes to exactly `MAX_KEYSET_BYTES`.
    let value = "x".repeat(MAX_KEYSET_BYTES - 4);
    let ks = Keyset::new([value.clone()], SortDir::Asc)
        .expect("the bound itself is admissible, not just values under it");
    assert_eq!(ks.values(), [value]);
}

#[test]
fn a_record_page_can_be_built_with_no_continuation() {
    let page = RecordPage {
        items: Vec::new(),
        next: None,
    };
    assert_eq!(
        page,
        RecordPage {
            items: Vec::new(),
            next: None,
        }
    );
}

/// `Keyset` must implement neither `Serialize` nor `Deserialize`: the wire
/// cursor is the gateway's to mint, so a keyset has no wire form, and spec
/// §12.1 requires the absence be pinned rather than left to review.
///
/// A trait-bound helper is the only way to state this in Rust. `NotSerialize`
/// is implemented for every type that is NOT `Serialize`, and `NotDeserialize`
/// likewise for every type that is NOT `Deserialize`; the moment someone
/// derives either on `Keyset`, the matching blanket impl below conflicts and
/// this module stops compiling with a coherence error naming the type. Two
/// separate shims because the two traits are independent — a derive of one
/// says nothing about the other, so pinning only one would leave a
/// `derive(Deserialize)` alone to slip through green.
#[test]
fn keyset_implements_no_serde() {
    // Compile-time assertion via a negative-reasoning shim. If `Keyset`
    // gains `Serialize`, `NotSerialize` has two applicable impls and the
    // crate fails to build — which is the loudest possible failure and the
    // one this pin wants. `dead_code` is allowed because nothing ever uses
    // `NotSerialize` as a bound: the two conflicting impls are the entire
    // assertion, and clippy cannot see that.
    #[allow(dead_code)]
    trait NotSerialize {}
    impl NotSerialize for Keyset {}
    impl<T: serde::Serialize> NotSerialize for T {}

    // Same shape, over `Deserialize<'de>` instead. `Deserialize` is
    // lifetime-generic, so the blanket impl is too; that does not weaken the
    // pin, since `Keyset`'s own impl still fixes the concrete type the
    // coherence check compares against.
    #[allow(dead_code)]
    trait NotDeserialize {}
    impl NotDeserialize for Keyset {}
    impl<'de, T: serde::Deserialize<'de>> NotDeserialize for T {}

    // The function body asserts nothing at runtime; the impls above are the
    // assertion. Named as a test so its purpose is discoverable and so
    // `cargo test` lists it.
    let _ = Keyset::new(["v"], SortDir::Asc).expect("one value is well-formed");
}

#[test]
fn a_keyset_naming_every_admissible_field_at_its_schema_cap_is_accepted() {
    // The watch item `MAX_KEYSET_BYTES` exists for: a caller can order by
    // every `KEYSET_SAFE_RECORD_FIELDS` entry at once, each exactly once
    // (nothing refuses such an order — uniform direction and no repeated key
    // are the only constraints a multi-key order has to meet), and two of
    // those fields — `resource_id` and `resource_type` — are caller-supplied
    // strings capped at 256 *characters* by `cap_attribution`, not 256
    // bytes. A caller whose resource id is 256 emoji (any 4-byte-encoded
    // Unicode scalar inflates the same way) is schema-legal and reachable
    // through ordinary ingestion, not forged input. This test proves the
    // bound against a real `serde_json` encoding rather than hand arithmetic.
    //
    // A 4-byte scalar is the widest an *admissible* value encodes to, which
    // is what makes this the worst case: `serde_json` escapes only `"`, `\`
    // and the ASCII controls, the first two at 2 bytes against this 4 and
    // the controls — 6 each — refused at construction.
    //
    // Driven from `KEYSET_SAFE_RECORD_FIELDS` itself rather than from seven
    // (now eight) hand-copied literal values: a field added to that constant
    // without a worst-case arm here panics the `other =>` branch below,
    // loudly and at the one site responsible for the bound's own arithmetic,
    // instead of leaving this test green while the real widest legal keyset
    // silently outgrows `MAX_KEYSET_BYTES` at runtime — which is exactly how
    // the constant went stale the first time: the test enumerated seven
    // literals, an eighth field (`accepted_at`) was admitted, and nothing
    // here would have noticed.
    let four_byte_char = '\u{1F600}'; // 😀 — 1 char, 4 UTF-8 bytes, no JSON escaping needed
    let attribution_at_cap: String = std::iter::repeat_n(four_byte_char, 256).collect();
    assert_eq!(
        attribution_at_cap.chars().count(),
        256,
        "precondition: exactly at cap_attribution's character cap, not bytes"
    );
    let uuid_width = "00000000-0000-0000-0000-000000000001".to_owned();
    let rfc3339_at_width = "2023-11-14T23:13:20.123456789Z".to_owned();
    let origin_worst = "backfill".to_owned(); // longer than "live"

    let values: Vec<String> = crate::v2::filter::KEYSET_SAFE_RECORD_FIELDS
        .iter()
        .map(|field| match *field {
            "resource_id" | "resource_type" => attribution_at_cap.clone(),
            "id" | "tenant_id" => uuid_width.clone(),
            "window_start" | "window_end" | "accepted_at" => rfc3339_at_width.clone(),
            "origin" => origin_worst.clone(),
            other => unreachable!(
                "KEYSET_SAFE_RECORD_FIELDS grew a field (`{other}`) this test does not yet \
                 assign a worst-case value; classify it above rather than leaving \
                 MAX_KEYSET_BYTES's own derivation test short of the real widest legal keyset"
            ),
        })
        .collect();

    // Measured, not assumed: this is what `KeysetInvalid::TooLarge::actual`
    // would report if the bound were too small, and it is what
    // the bound is pinned against the real encoder.
    assert_eq!(
        serde_json::to_string(&values)
            .expect("Vec<String> always serializes")
            .len(),
        2243,
        "if this changes, `MAX_KEYSET_BYTES` is stale and must be \
         re-measured, not just bumped",
    );

    Keyset::new(values, SortDir::Asc)
        .expect("the widest legal keyset this schema admits must fit the budget");
}
