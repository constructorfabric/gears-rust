use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use super::*;

const FINGERPRINT: &[u8] = &[7; 32];

/// A managed token laid out by hand: `version || digest`.
fn token(version: u8, digest: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode([&[version], digest].concat())
}

fn digest_of(validator: Validator) -> Vec<u8> {
    let wire = URL_SAFE_NO_PAD
        .decode(validator.encode())
        .expect("base64url");
    wire[1..].to_vec()
}

/// The golden wire-form test pins a Type Schema; this covers an Instance.
#[test]
fn equal_instance_inputs_give_a_byte_identical_token() {
    let a = Validator::compute(3, None);
    let b = Validator::compute(3, None);
    assert_eq!(a.encode(), b.encode());
}

#[test]
fn a_revision_changes_the_validator() {
    let before = Validator::compute(3, Some(FINGERPRINT));
    let after = Validator::compute(4, Some(FINGERPRINT));
    assert_ne!(before, after);
}

#[test]
fn a_refreshed_fingerprint_changes_the_validator_at_the_same_revision() {
    let before = Validator::compute(3, Some(FINGERPRINT));
    let after = Validator::compute(3, Some(&[8; 32]));
    assert_ne!(before, after);
}

#[test]
fn an_instance_validator_has_no_fingerprint_and_still_changes_on_revision() {
    let before = Validator::compute(1, None);
    let after = Validator::compute(2, None);
    assert_ne!(before, after);
    assert_ne!(before, Validator::compute(1, Some(&[])));
}

#[test]
fn the_wire_form_is_base64url_of_the_version_byte_and_the_digest() {
    let encoded = Validator::compute(3, Some(FINGERPRINT)).encode();
    // Computed outside this crate, so it pins the version byte, its place in the
    // hash, the digest order and the base64url form rather than echoing `encode`.
    assert_eq!(encoded, "Anl11EJWAxpvuinixMx1Fm0");
    let wire = URL_SAFE_NO_PAD.decode(&encoded).expect("base64url");
    assert_eq!(
        (wire[0], wire.len()),
        (2, 17),
        "a version byte, then a 128-bit digest"
    );
}

#[test]
fn a_token_decodes_back_to_its_validator() {
    for validator in [
        Validator::compute(3, None),
        Validator::compute(1, Some(FINGERPRINT)),
        Validator::compute(i64::MAX, Some(&[])),
    ] {
        assert_eq!(Validator::decode(&validator.encode()), Some(validator));
    }
}

/// Any 128-bit digest under the current version byte is a token, and decodes to
/// the validator that encodes back to it: the wire form loses nothing.
#[test]
fn every_digest_round_trips_through_the_wire_form() {
    let ascending: Vec<u8> = (0..16).collect();
    for digest in [[0_u8; 16].to_vec(), [0xff; 16].to_vec(), ascending] {
        let wire = token(2, &digest);
        let validator = Validator::decode(&wire).expect("a well-formed token");
        assert_eq!(validator.encode(), wire);
        assert_eq!(digest_of(validator), digest);
    }
}

#[test]
fn an_unknown_version_is_not_a_match() {
    let validator = Validator::compute(3, Some(FINGERPRINT));
    assert_eq!(Validator::decode(&token(3, &digest_of(validator))), None);
}

/// v1 also digested the selected fields, so its tokens describe one projection only;
/// a client still holding one gets a full result rather than a match.
#[test]
fn a_v1_token_is_not_a_match() {
    let validator = Validator::compute(3, Some(FINGERPRINT));
    assert_eq!(Validator::decode(&token(1, &digest_of(validator))), None);
}

#[test]
fn a_malformed_token_is_not_a_match() {
    for bad in [
        String::new(),
        "not base64!".to_owned(),
        URL_SAFE_NO_PAD.encode([1_u8]),
        token(2, &[0; 8]),
        token(2, &[0; 17]),
    ] {
        assert_eq!(Validator::decode(&bad), None, "{bad:?}");
    }
}

/// The JSON envelope this form replaced: a client still holding one gets a full
/// result rather than a match or an error.
#[test]
fn a_json_envelope_token_is_not_a_match() {
    let validator = Validator::compute(3, Some(FINGERPRINT));
    let digest = URL_SAFE_NO_PAD.encode(digest_of(validator));
    let json = URL_SAFE_NO_PAD.encode(format!(r#"{{"v":1,"d":"{digest}"}}"#));
    assert_eq!(Validator::decode(&json), None);
}
