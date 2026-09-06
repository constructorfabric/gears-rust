// Created: 2026-08-11 by Constructor Tech
//! Tests for the setting key value object.
//!
//! Contract source: ADR-002 and DESIGN.md §3 *Setting key by author* — the
//! setting key is a GTS type id `<base>~<derived>~`, admin derived half
//! `<vendor>.settings.<category>.<name>.v1~`.
//!
//! Acceptance criteria: FEATURE `gear-foundation.md` §6.

use std::str::FromStr;

use super::{SETTING_TYPE_BASE, SettingKey, SettingKeyError};

/// A well-formed admin key.
const VALID_KEY: &str =
    "gts.cf.core.settings.setting_type.v1~acme.settings.network.enable_proxy.v1~";
const VALID_DERIVED: &str = "acme.settings.network.enable_proxy.v1~";

#[test]
fn parses_base_and_derived_halves() {
    let key = SettingKey::parse(VALID_KEY).expect("well-formed key must parse");
    assert_eq!(key.base_type(), SETTING_TYPE_BASE);
    assert_eq!(key.derived_half(), VALID_DERIVED);
}

#[test]
fn both_halves_are_types_and_keep_their_terminator() {
    // A setting is a GTS *type* so that a policy can name it as a resource; an
    // instance-shaped half would be a key nothing can be authorized against.
    let key = SettingKey::parse(VALID_KEY).expect("well-formed key must parse");
    assert!(key.base_type().ends_with('~'));
    assert!(key.derived_half().ends_with('~'));
}

#[test]
fn the_derived_half_does_not_repeat_the_gts_prefix() {
    // Only the first segment carries `gts.`; repeating it in the second would
    // count as a name token and push the segment over the four-token grammar.
    let key = SettingKey::parse(VALID_KEY).expect("well-formed key must parse");
    assert!(!key.derived_half().starts_with("gts."));
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
fn rejects_the_bare_base_type() {
    // One segment is the abstract base, not a setting.
    let err = SettingKey::parse(SETTING_TYPE_BASE).expect_err("the bare base is not a key");
    assert_eq!(err, SettingKeyError::SegmentCount { count: 1 });
}

#[test]
fn rejects_a_chain_with_three_segments() {
    let err = SettingKey::parse(&format!("{VALID_KEY}c.d.e.f.v1~"))
        .expect_err("a setting key is exactly the base plus one derived half");
    assert_eq!(err, SettingKeyError::SegmentCount { count: 3 });
}

#[test]
fn rejects_a_key_rooted_under_another_base() {
    // Every setting derives from the one base this gear registers. A key rooted
    // anywhere else — here, under a value type — is not a setting whatever else
    // it may be.
    let err = SettingKey::parse(&format!(
        "gts.cf.toolkit.settings.type_bool_flag.v1~{VALID_DERIVED}"
    ))
    .expect_err("a value type is not the setting base");
    assert_eq!(
        err,
        SettingKeyError::WrongBaseType {
            found: "gts.cf.toolkit.settings.type_bool_flag.v1~".to_owned()
        }
    );
}

#[test]
fn rejects_a_derived_half_without_terminator() {
    // Dropping the trailing `~` turns the derived half into an instance — the
    // shape the retired ADR-001 used, and the one a policy cannot target.
    let err = SettingKey::parse(VALID_KEY.trim_end_matches('~'))
        .expect_err("an instance-shaped derived half is not a setting key");
    assert_eq!(err, SettingKeyError::DerivedNotAType);
}

#[test]
fn rejects_identifier_without_gts_prefix() {
    let err = SettingKey::parse("cf.core.settings.setting_type.v1~acme.settings.net.x.v1~")
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
    let err = SettingKey::parse(&format!("{SETTING_TYPE_BASE}Acme.settings.net.x.v1~"))
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
    let err = SettingKey::parse(&format!("{SETTING_TYPE_BASE}acme.settings.net/work.x.v1~"))
        .expect_err("`/` is reserved and never valid");
    match err {
        SettingKeyError::InvalidSegment { num, segment, .. } => {
            assert_eq!(num, 2, "the offending segment is the derived half");
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
    // Five name tokens before the version breaks the GTS grammar.
    let err = SettingKey::parse(&format!("{SETTING_TYPE_BASE}acme.settings.net.sub.x.v1~"))
        .expect_err("a GTS segment carries exactly four name tokens");
    match err {
        SettingKeyError::InvalidSegment { num, cause, .. } => {
            assert_eq!(num, 2);
            assert!(
                cause.contains("tokens"),
                "the cause should name the token count, got `{cause}`"
            );
        }
        other => panic!("expected InvalidSegment, got {other:?}"),
    }
}

#[test]
fn composes_admin_key_in_the_design_shape() {
    let key = SettingKey::compose("acme", "network", "enable_proxy")
        .expect("well-formed inputs must compose");
    assert_eq!(key.as_str(), VALID_KEY);
}

#[test]
fn compose_takes_no_value_type() {
    // The value's shape is a separate catalog type named by `value_type_id`, so
    // the same key must survive a value-shape change. Two settings that differ
    // only in value type therefore compose to the *same* key -- uniqueness is a
    // property of the identity, not of the shape.
    let key = SettingKey::compose("acme", "network", "enable_proxy").expect("composes");
    assert!(!key.as_str().contains("type_bool_flag"));
}

#[test]
fn compose_rejects_uppercase_vendor() {
    let err = SettingKey::compose("Acme", "network", "enable_proxy")
        .expect_err("vendor must be lowercase");
    assert!(matches!(err, SettingKeyError::InvalidId { .. }));
}

#[test]
fn compose_rejects_category_containing_reserved_separator() {
    let err = SettingKey::compose("acme", "net/work", "enable_proxy")
        .expect_err("`/` is reserved and never valid in a category slug");
    assert!(matches!(err, SettingKeyError::InvalidSegment { .. }));
}

#[test]
fn exposes_category_and_leaf_from_the_derived_half() {
    // `UNIQUE(category_id, leaf_slug)` is enforced on the leaf, so it must be recoverable.
    let key = SettingKey::parse(VALID_KEY).expect("well-formed key must parse");
    assert_eq!(key.category_slug(), "network");
    assert_eq!(key.leaf_slug(), "enable_proxy");
}

#[test]
fn module_supplied_key_exposes_the_same_positions() {
    // The reconciler reads a module's category from the third token, the same
    // place an admin key puts it; only the package differs.
    let key = SettingKey::parse(&format!("{SETTING_TYPE_BASE}acme.mymod.queues.retries.v1~"))
        .expect("module-supplied key must parse");
    assert_eq!(key.category_slug(), "queues");
    assert_eq!(key.leaf_slug(), "retries");
}

#[test]
fn recategorizing_produces_a_different_key() {
    // The category is embedded, so a move re-keys the setting with no alias.
    let before = SettingKey::compose("acme", "network", "enable_proxy").expect("composes");
    let after = SettingKey::compose("acme", "security", "enable_proxy").expect("composes");
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
fn an_anonymous_derived_half_is_refused() {
    // `gts-id` allows a trailing UUID tail for machine-generated *instances*,
    // never for a type: a type segment may not contain `-`, so a UUID-tailed
    // derived half is refused by the grammar before this module sees it. The
    // `AnonymousDerivedHalf` arm remains as defence in depth -- if the grammar
    // ever admitted one, a key with an empty category and leaf would still be
    // refused rather than stored.
    let key = format!("{SETTING_TYPE_BASE}550e8400-e29b-41d4-a716-446655440000~");
    let result = SettingKey::parse(&key);
    assert!(
        matches!(
            result,
            Err(SettingKeyError::AnonymousDerivedHalf | SettingKeyError::InvalidId { .. })
        ),
        "got {result:?}"
    );
}

#[test]
fn surrounding_whitespace_is_refused_rather_than_trimmed() {
    // The GTS parser trims before validating. This type stores the candidate
    // verbatim, so a trimmed-then-accepted key would give one setting two
    // spellings and shift the split point.
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
