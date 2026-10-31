//! RFC 8785 conformance and the secret hash.

use secrecy::SecretString;
use serde_json::{Value, json};

use super::{ExpectedValue, SECRET_HASH_DOMAIN, canonical_json, plaintext_text, secret_hash};

fn canonical(text: &str) -> String {
    let value: Value = serde_json::from_str(text).expect("valid JSON");
    canonical_json(&value).expect("canonical")
}

#[test]
fn rfc_8785_appendix_b_numbers_are_written_as_ecmascript_writes_them() {
    // The IEEE-754 samples of RFC 8785 Appendix B, by bit pattern.
    let samples: [(u64, &str); 24] = [
        (0x0000_0000_0000_0000, "0"),
        (0x8000_0000_0000_0000, "0"),
        (0x0000_0000_0000_0001, "5e-324"),
        (0x8000_0000_0000_0001, "-5e-324"),
        (0x7fef_ffff_ffff_ffff, "1.7976931348623157e+308"),
        (0xffef_ffff_ffff_ffff, "-1.7976931348623157e+308"),
        (0x4340_0000_0000_0000, "9007199254740992"),
        (0xc340_0000_0000_0000, "-9007199254740992"),
        (0x4430_0000_0000_0000, "295147905179352830000"),
        (0x44b5_2d02_c7e1_4af5, "9.999999999999997e+22"),
        (0x44b5_2d02_c7e1_4af6, "1e+23"),
        (0x44b5_2d02_c7e1_4af7, "1.0000000000000001e+23"),
        (0x444b_1ae4_d6e2_ef4e, "999999999999999700000"),
        (0x444b_1ae4_d6e2_ef4f, "999999999999999900000"),
        (0x444b_1ae4_d6e2_ef50, "1e+21"),
        (0x3eb0_c6f7_a0b5_ed8c, "9.999999999999997e-7"),
        (0x3eb0_c6f7_a0b5_ed8d, "0.000001"),
        (0x41b3_de43_5555_5553, "333333333.3333332"),
        (0x41b3_de43_5555_5554, "333333333.33333325"),
        (0x41b3_de43_5555_5555, "333333333.3333333"),
        (0x41b3_de43_5555_5556, "333333333.3333334"),
        (0x41b3_de43_5555_5557, "333333333.33333343"),
        (0x4340_0000_0000_0001, "9007199254740994"),
        (0x4314_3ff3_c1cb_0959, "1424953923781206.2"),
    ];
    for (bits, expected) in samples {
        let double = f64::from_bits(bits);
        let value = json!(double);
        assert_eq!(
            canonical_json(&value).expect("a finite double"),
            expected,
            "bits {bits:#018x}"
        );
    }
}

#[test]
fn rfc_8785_section_3_2_2_example_is_reproduced_byte_for_byte() {
    let input = r#"{
        "numbers": [333333333.33333329, 1E30, 4.50, 2e-3, 0.000000000000000000000000001],
        "string": "\u20ac$\u000F\u000aA'\u0042\u0022\u005c\\\"\/",
        "literals": [null, true, false]
    }"#;
    assert_eq!(
        canonical(input),
        "{\"literals\":[null,true,false],\"numbers\":[333333333.3333333,1e+30,4.5,0.002,1e-27],\
         \"string\":\"\u{20ac}$\\u000f\\nA'B\\\"\\\\\\\\\\\"/\"}"
    );
}

#[test]
fn rfc_8785_section_3_2_3_orders_members_by_utf16_code_units() {
    let input = r#"{
        "\u20ac": "Euro Sign",
        "\r": "Carriage Return",
        "\ufb33": "Hebrew Letter Dalet With Dagesh",
        "1": "One",
        "\ud83d\ude00": "Emoji: Grinning Face",
        "\u0080": "Control",
        "\u00f6": "Latin Small Letter O With Diaeresis"
    }"#;
    let value: Value = serde_json::from_str(input).expect("valid JSON");
    let encoded = canonical_json(&value).expect("canonical");
    let decoded: Value = serde_json::from_str(&encoded).expect("canonical text is JSON");
    let order = |names: &[&str]| -> Vec<usize> {
        names
            .iter()
            .map(|n| {
                let literal = canonical_json(&json!(n)).expect("a string");
                encoded.find(&format!("{literal}:")).expect("name present")
            })
            .collect()
    };
    // The RFC's expected order: CR, "1", U+0080, U+00F6, U+20AC, U+1F600
    // (a surrogate pair, below U+FB33 in UTF-16), U+FB33.
    let positions = order(&[
        "\r",
        "1",
        "\u{80}",
        "\u{f6}",
        "\u{20ac}",
        "\u{1f600}",
        "\u{fb33}",
    ]);
    let mut sorted = positions.clone();
    sorted.sort_unstable();
    assert_eq!(positions, sorted, "{encoded}");
    assert_eq!(decoded, value, "the encoding denotes the same value");
}

#[test]
fn control_characters_use_the_short_form_or_lowercase_unicode_escapes() {
    let value = json!("\u{8}\u{9}\u{a}\u{c}\u{d}\u{1}\u{1f}\u{7f}/\u{e9}\u{1f600}");
    assert_eq!(
        canonical_json(&value).expect("canonical"),
        "\"\\b\\t\\n\\f\\r\\u0001\\u001f\u{7f}/\u{e9}\u{1f600}\""
    );
}

#[test]
fn a_value_reserialized_any_way_encodes_to_the_same_bytes() {
    // Member order, whitespace, number spelling and Unicode escaping all vary.
    let recorded =
        canonical("{\"b\":[1.0,2e0,\"\\u00e9\"],\"a\":{\"y\":true,\"x\":null},\"c\":1.5}");
    let reserialized = canonical(
        "{ \"c\" : 1.50 , \"a\" : { \"x\" : null , \"y\" : true } ,\n \
         \"b\" : [ 1 , 2.0 , \"\\u00e9\" ] }",
    );
    assert_eq!(recorded, reserialized);
    assert_eq!(
        recorded,
        "{\"a\":{\"x\":null,\"y\":true},\"b\":[1,2,\"\u{e9}\"],\"c\":1.5}"
    );
}

#[test]
fn a_file_reference_value_is_an_ordinary_object_with_no_hash() {
    let reference = json!({ "file_id": "f-1", "version": 3, "name": "ca.pem" });
    let expected = ExpectedValue::of_value(&reference).expect("encodes");
    assert_eq!(
        expected.as_str(),
        r#"{"file_id":"f-1","name":"ca.pem","version":3}"#
    );
}

#[test]
fn a_number_outside_the_binary64_domain_is_refused_not_rounded() {
    let beyond: Value = serde_json::from_str("9007199254740993").expect("valid JSON");
    assert!(canonical_json(&beyond).is_err(), "2^53 + 1 is refused");
    let negative: Value = serde_json::from_str("-9007199254740993").expect("valid JSON");
    assert!(canonical_json(&negative).is_err());
    let edge: Value = serde_json::from_str("9007199254740992").expect("valid JSON");
    assert_eq!(
        canonical_json(&edge).expect("2^53 is exact"),
        "9007199254740992"
    );
    let decimal: Value = serde_json::from_str("1.10").expect("valid JSON");
    assert_eq!(canonical_json(&decimal).expect("1.1"), "1.1");
}

#[test]
fn a_secret_hash_is_sha256_hex_over_the_canonical_array() {
    let key = "gts.cf.core.settings.setting_type.v1~acme.net.proxy.password.v1~";
    let tenant = "11111111-1111-1111-1111-111111111111";
    let plaintext = SecretString::from("hunter2");
    let hash = secret_hash(key, tenant, &plaintext).expect("hashes");
    assert_eq!(hash.len(), 64);
    assert!(
        hash.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
        "lowercase hex: {hash}"
    );
    // Recomputed from the definition: the canonical array, hashed.
    let array = format!("[\"{SECRET_HASH_DOMAIN}\",\"{key}\",\"{tenant}\",\"hunter2\"]");
    let by_hand = {
        use aws_lc_rs::digest::{SHA256, digest};
        hex::encode(digest(&SHA256, array.as_bytes()).as_ref())
    };
    assert_eq!(hash, by_hand);
}

#[test]
fn the_same_secret_hashes_differently_by_key_and_tenant_and_equally_across_changes() {
    let plaintext = SecretString::from("hunter2");
    let again = SecretString::from("hunter2");
    let base = secret_hash("k1", "t1", &plaintext).expect("hashes");
    assert_eq!(base, secret_hash("k1", "t1", &again).expect("hashes"));
    assert_ne!(base, secret_hash("k2", "t1", &plaintext).expect("hashes"));
    assert_ne!(base, secret_hash("k1", "t2", &plaintext).expect("hashes"));
    assert_ne!(
        base,
        secret_hash("k1", "t1", &SecretString::from("hunter3")).expect("hashes")
    );
}

#[test]
fn a_secret_is_hashed_from_the_text_the_store_holds() {
    // A string as itself, anything else as its JSON text: the bytes the
    // Credential Store keeps and hands back.
    use secrecy::ExposeSecret;
    assert_eq!(plaintext_text(&json!("abc")).expose_secret(), "abc");
    assert_eq!(
        plaintext_text(&json!({ "a": 1 })).expose_secret(),
        "{\"a\":1}"
    );
    assert_eq!(
        ExpectedValue::of_secret("k", "t", &plaintext_text(&json!("abc")))
            .expect("hashes")
            .as_str(),
        secret_hash("k", "t", &SecretString::from("abc")).expect("hashes")
    );
}

#[test]
fn a_secret_hash_matches_a_known_answer_computed_outside_this_crate() {
    // SHA-256 of `["cf.settings.activation.secret.v1","k","t","hunter2"]`,
    // computed with Python's `hashlib`: the scheme's wire value, pinned.
    assert_eq!(
        secret_hash("k", "t", &SecretString::from("hunter2".to_owned())).expect("hashes"),
        "db8d227c5d9e88a8401002a0ea32eea050b860f27113f2f0f9223ea99d668bd0"
    );
}

// ---------------------------------------------------------------------------
// The shared corpus
// ---------------------------------------------------------------------------

/// The conformance corpus both sides of a report run: the SDK here, the gear
/// against the `expected_value` it stores. One file, so the two cannot drift.
const CORPUS: &str = include_str!("canonical_corpus.json");

fn corpus() -> Value {
    serde_json::from_str(CORPUS).expect("the corpus is JSON")
}

#[test]
fn the_shared_corpus_encodes_to_its_known_bytes() {
    let corpus = corpus();
    let values = corpus["values"].as_array().expect("values");
    assert!(values.len() >= 8, "the corpus lost cases");
    for case in values {
        let input = case["input"].as_str().expect("input");
        let expected = case["canonical"].as_str().expect("canonical");
        let value: Value = serde_json::from_str(input).expect("the input is JSON");
        assert_eq!(
            canonical_json(&value).expect("canonical"),
            expected,
            "{}",
            case["name"]
        );
        // Encoding the canonical text again changes nothing: it is a fixed point.
        let again: Value = serde_json::from_str(expected).expect("canonical text is JSON");
        assert_eq!(
            canonical_json(&again).expect("canonical"),
            expected,
            "{} is not a fixed point",
            case["name"]
        );
    }
}

#[test]
fn the_shared_corpus_refuses_what_binary64_cannot_carry() {
    let corpus = corpus();
    for case in corpus["rejected"].as_array().expect("rejected") {
        let value: Value =
            serde_json::from_str(case["input"].as_str().expect("input")).expect("JSON");
        assert!(canonical_json(&value).is_err(), "{}", case["name"]);
    }
}

#[test]
fn the_shared_corpus_secret_hashes_match_answers_computed_outside_the_crate() {
    let corpus = corpus();
    for case in corpus["secrets"].as_array().expect("secrets") {
        let plaintext = SecretString::from(case["plaintext"].as_str().expect("plaintext"));
        assert_eq!(
            secret_hash(
                case["key"].as_str().expect("key"),
                case["tenant"].as_str().expect("tenant"),
                &plaintext
            )
            .expect("hashes"),
            case["hash"].as_str().expect("hash"),
            "{}",
            case["name"]
        );
    }
}
