// Created: 2026-08-11 by Constructor Tech
//! Tests for the setting key value object.
//!
//! Contract source: `ADR-001-setting-key-gts-instance-id` — the setting key is a
//! GTS instance id `<value-type>~<setting-instance-id>`, admin instance segment
//! `<vendor>.settings.<category>.<name>.v1`.
//!
//! Acceptance criteria: FEATURE `gear-foundation.md` §6.

use std::str::FromStr;

use super::{SettingKey, SettingKeyError};

/// A well-formed admin key, per ADR-001 decision point 3.
const VALID_KEY: &str = "gts.cf.settings.types.bool_flag.v1~acme.settings.network.enable_proxy.v1";
const VALID_VALUE_TYPE: &str = "gts.cf.settings.types.bool_flag.v1~";
const VALID_INSTANCE: &str = "acme.settings.network.enable_proxy.v1";

#[test]
fn parses_value_type_and_instance_halves() {
    let key = SettingKey::parse(VALID_KEY).expect("well-formed key must parse");
    assert_eq!(key.value_type(), VALID_VALUE_TYPE);
    assert_eq!(key.instance_id(), VALID_INSTANCE);
}

#[test]
fn value_type_half_retains_its_terminator() {
    let key = SettingKey::parse(VALID_KEY).expect("well-formed key must parse");
    assert!(
        key.value_type().ends_with('~'),
        "the value type is a GTS type and must keep its trailing `~`"
    );
}

#[test]
fn instance_half_carries_no_terminator() {
    let key = SettingKey::parse(VALID_KEY).expect("well-formed key must parse");
    assert!(
        !key.instance_id().ends_with('~'),
        "the setting is a GTS instance and must not end with `~`"
    );
}

#[test]
fn instance_half_does_not_repeat_the_gts_prefix() {
    // Only the first segment carries `gts.`; repeating it in the second would
    // count as a name token and push the segment over the four-token grammar.
    let key = SettingKey::parse(VALID_KEY).expect("well-formed key must parse");
    assert!(
        !key.instance_id().starts_with("gts."),
        "the instance segment follows the `~`, so it must not repeat the prefix"
    );
}

#[test]
fn round_trips_byte_identically() {
    let key = SettingKey::parse(VALID_KEY).expect("well-formed key must parse");
    assert_eq!(
        key.as_str(),
        VALID_KEY,
        "parsing must not trim, lowercase, or otherwise normalize the key"
    );
}

#[test]
fn rejects_a_bare_value_type() {
    // One segment is a type, not a setting key.
    let err = SettingKey::parse(VALID_VALUE_TYPE).expect_err("a bare value type is not a key");
    assert_eq!(err, SettingKeyError::SegmentCount { count: 1 });
}

#[test]
fn rejects_a_chained_key_with_three_segments() {
    let err =
        SettingKey::parse("gts.cf.settings.types.bool_flag.v1~acme.settings.a.b.v1~c.d.e.f.v1")
            .expect_err("a setting key is exactly a value type plus an instance");
    assert_eq!(err, SettingKeyError::SegmentCount { count: 3 });
}

#[test]
fn rejects_instance_half_with_trailing_separator() {
    let err = SettingKey::parse(&format!("{VALID_KEY}~"))
        .expect_err("a trailing `~` makes the right half a type, not an instance");
    assert_eq!(err, SettingKeyError::TrailingSeparator);
}

#[test]
fn rejects_identifier_without_gts_prefix() {
    let err = SettingKey::parse("cf.settings.types.bool_flag.v1~acme.settings.net.x.v1")
        .expect_err("a setting key is a GTS identifier");
    match err {
        SettingKeyError::InvalidId { cause } => assert!(
            cause.contains("gts."),
            "the cause should name the missing prefix, got `{cause}`"
        ),
        other => panic!("expected InvalidId, got {other:?}"),
    }
}

#[test]
fn rejects_uppercase() {
    let err = SettingKey::parse("gts.cf.settings.types.bool_flag.v1~Acme.settings.net.x.v1")
        .expect_err("GTS identifiers are lowercase");
    match err {
        SettingKeyError::InvalidId { cause } => assert!(
            cause.contains("lowercase"),
            "the cause should name the case rule, got `{cause}`"
        ),
        other => panic!("expected InvalidId, got {other:?}"),
    }
}

#[test]
fn rejects_reserved_path_separator_naming_the_segment() {
    let err = SettingKey::parse("gts.cf.settings.types.bool_flag.v1~acme.settings.net/work.x.v1")
        .expect_err("`/` is reserved and never valid");
    match err {
        SettingKeyError::InvalidSegment { num, segment, .. } => {
            assert_eq!(num, 2, "the offending segment is the instance half");
            assert!(
                segment.contains('/'),
                "the error must name the offending segment, got `{segment}`"
            );
        }
        other => panic!("expected InvalidSegment, got {other:?}"),
    }
}

#[test]
fn rejects_too_many_name_tokens() {
    // Five name tokens before the version breaks the GTS grammar; this is what
    // the pre-`gts-id` hand-rolled validator silently accepted.
    let err =
        SettingKey::parse("gts.cf.toolkit.settings.types.bool_flag.v1~acme.settings.net.x.v1")
            .expect_err("a GTS segment carries exactly four name tokens");
    match err {
        SettingKeyError::InvalidSegment { num, cause, .. } => {
            assert_eq!(num, 1);
            assert!(
                cause.contains("tokens"),
                "the cause should name the token count, got `{cause}`"
            );
        }
        other => panic!("expected InvalidSegment, got {other:?}"),
    }
}

#[test]
fn composes_admin_key_in_the_adr_shape() {
    let key = SettingKey::compose(VALID_VALUE_TYPE, "acme", "network", "enable_proxy")
        .expect("well-formed inputs must compose");
    assert_eq!(key.as_str(), VALID_KEY);
}

#[test]
fn compose_rejects_value_type_without_terminator() {
    let err = SettingKey::compose("gts.cf.settings.types.bool_flag.v1", "acme", "n", "x")
        .expect_err("a value type must be terminated by `~`");
    assert_eq!(err, SettingKeyError::ValueTypeNotAType);
}

#[test]
fn compose_rejects_uppercase_vendor() {
    let err = SettingKey::compose(VALID_VALUE_TYPE, "Acme", "network", "enable_proxy")
        .expect_err("vendor must be lowercase");
    assert!(matches!(err, SettingKeyError::InvalidId { .. }));
}

#[test]
fn compose_rejects_category_containing_reserved_separator() {
    let err = SettingKey::compose(VALID_VALUE_TYPE, "acme", "net/work", "enable_proxy")
        .expect_err("`/` is reserved and never valid in a category slug");
    assert!(matches!(err, SettingKeyError::InvalidSegment { .. }));
}

#[test]
fn exposes_category_and_leaf_from_the_instance_segment() {
    // `UNIQUE(category_id, leaf_slug)` is enforced on the leaf, so it must be recoverable.
    let key = SettingKey::parse(VALID_KEY).expect("well-formed key must parse");
    assert_eq!(key.category_slug(), "network");
    assert_eq!(key.leaf_slug(), "enable_proxy");
}

#[test]
fn module_supplied_key_exposes_the_same_positions() {
    // ADR-001 decision point 4: the reconciler reads a module's category from the
    // namespace position, the same place an admin key puts it.
    let key = SettingKey::parse("gts.cf.settings.types.bool_flag.v1~acme.mymod.queues.retries.v1")
        .expect("module-supplied key must parse");
    assert_eq!(key.category_slug(), "queues");
    assert_eq!(key.leaf_slug(), "retries");
}

#[test]
fn recategorizing_produces_a_different_key() {
    // ADR-001 decision point 6: the category is embedded, so a move re-keys the setting.
    let before =
        SettingKey::compose(VALID_VALUE_TYPE, "acme", "network", "enable_proxy").expect("composes");
    let after = SettingKey::compose(VALID_VALUE_TYPE, "acme", "security", "enable_proxy")
        .expect("composes");
    assert_ne!(before.as_str(), after.as_str());
}

#[test]
fn from_str_matches_parse() {
    let via_parse = SettingKey::parse(VALID_KEY).expect("parses");
    let via_from_str = SettingKey::from_str(VALID_KEY).expect("parses");
    assert_eq!(via_parse, via_from_str);
}

#[test]
fn serializes_as_the_bare_key_string() {
    let key = SettingKey::parse(VALID_KEY).expect("parses");
    let json = serde_json::to_string(&key).expect("serializes");
    assert_eq!(json, format!("\"{VALID_KEY}\""));
}

#[test]
fn deserializes_through_parse() {
    let json = format!("\"{VALID_KEY}\"");
    let key: SettingKey = serde_json::from_str(&json).expect("deserializes");
    assert_eq!(key.as_str(), VALID_KEY);
}

#[test]
fn deserializing_a_malformed_key_fails() {
    // A `SettingKey` that would not round-trip must never enter the type.
    let err = serde_json::from_str::<SettingKey>("\"not-a-key\"")
        .expect_err("a malformed key must be rejected at deserialization");
    assert!(
        !err.to_string().is_empty(),
        "the parse error should surface"
    );
}

#[test]
fn an_anonymous_uuid_instance_is_refused() {
    // `gts-id` allows a trailing UUID tail for machine-generated instances, and
    // answers `""` for its namespace and type tokens. Accepting one would build
    // a key with an empty category and leaf that still round-tripped and still
    // compared equal to itself — a setting nobody could find by name.
    let key = format!("{VALID_VALUE_TYPE}550e8400-e29b-41d4-a716-446655440000");
    assert!(matches!(
        SettingKey::parse(&key),
        Err(SettingKeyError::AnonymousInstance)
    ));
}

#[test]
fn surrounding_whitespace_is_refused_rather_than_trimmed() {
    // The GTS parser trims before validating. This type stores the candidate
    // verbatim, so a trimmed-then-accepted key would give one setting two
    // spellings and shift the value-type split point.
    for candidate in [
        format!(" {VALID_KEY}"),
        format!("{VALID_KEY} "),
        format!("\t{VALID_KEY}\n"),
    ] {
        assert!(
            matches!(
                SettingKey::parse(&candidate),
                Err(SettingKeyError::SurroundingWhitespace)
            ),
            "`{candidate:?}` must be refused"
        );
    }
}

#[test]
fn a_padded_key_never_becomes_a_second_spelling_of_a_valid_one() {
    // The consequence the guard above exists to prevent, stated directly.
    let padded = format!(" {VALID_KEY}");
    assert!(SettingKey::parse(VALID_KEY).is_ok());
    assert!(SettingKey::parse(&padded).is_err());
}
