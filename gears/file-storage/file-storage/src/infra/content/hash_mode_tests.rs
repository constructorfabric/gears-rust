//! Wire-format tests for [`Manifest`] (ADR-0006 §3). Per the staged plan,
//! this is "the single most important test in the project" — the concrete
//! proof that the manifest grammar is byte-for-byte unambiguous across
//! independent implementations.

use sha2::{Digest, Sha256};

use super::*;

fn digest_of(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

fn sample_entries() -> Vec<ManifestEntry> {
    vec![
        ManifestEntry {
            offset: 0,
            digest: digest_of(b"part-zero"),
        },
        ManifestEntry {
            offset: 8_388_608, // 8 MiB
            digest: digest_of(b"part-one"),
        },
        ManifestEntry {
            offset: 16_777_216, // 16 MiB
            digest: digest_of(b"part-two-tail"),
        },
    ]
}

/// Hand-written expected wire string for [`sample_entries`], built without
/// going through `Manifest` at all — an independent encoding using only
/// `hex::encode` and plain string formatting, so this test cannot pass by
/// tautology against `to_wire_string`'s own implementation.
fn expected_wire_string(entries: &[ManifestEntry]) -> String {
    let mut s = String::from("v1");
    for e in entries {
        s.push(',');
        s.push_str(&e.offset.to_string());
        s.push(':');
        s.push_str(&hex::encode(e.digest));
    }
    s
}

// ── (a) fixed (offset, digest) pairs → exact expected wire string ─────────

#[test]
fn to_wire_string_matches_hand_written_expected_string_exactly() {
    let entries = sample_entries();
    let manifest = Manifest::new(entries.clone()).expect("valid manifest");
    let expected = expected_wire_string(&entries);
    assert_eq!(manifest.to_wire_string(), expected);

    // Spot-check the exact literal shape too, not just self-consistency
    // against the `expected_wire_string` helper.
    let d0 = hex::encode(entries[0].digest);
    let d1 = hex::encode(entries[1].digest);
    let d2 = hex::encode(entries[2].digest);
    assert_eq!(
        manifest.to_wire_string(),
        format!("v1,0:{d0},8388608:{d1},16777216:{d2}")
    );
}

/// Single-part manifest (the minimum-degenerate case per §3 rule 8).
///
/// The **type** still encodes a one-entry manifest (`v1,0:<digest>`) — the
/// grammar has no special case — but the ADR-0006 single-part amendment
/// means `complete_multipart_upload` never persists or returns one: a
/// one-part plan finalizes as `whole-sha256` with no manifest at all (see
/// `multipart_service.rs`). This test pins the wire encoding as a type-level
/// property only.
#[test]
fn to_wire_string_single_part_minimal_shape() {
    let digest = digest_of(b"only-part");
    let manifest = Manifest::new(vec![ManifestEntry { offset: 0, digest }]).unwrap();
    assert_eq!(
        manifest.to_wire_string(),
        format!("v1,0:{}", hex::encode(digest))
    );
}

// ── (b) sha256(to_wire_string()) matches an independently-computed root ────

#[test]
fn root_matches_independently_computed_reference_sha256() {
    let entries = sample_entries();
    let manifest = Manifest::new(entries.clone()).unwrap();
    let wire = expected_wire_string(&entries);

    // Independent reference: hash the hand-written string directly with a
    // fresh `Sha256` instance, not through `Manifest::root()`.
    let reference_root: [u8; 32] = {
        let mut hasher = Sha256::new();
        hasher.update(wire.as_bytes());
        hasher.finalize().into()
    };

    assert_eq!(manifest.root(), reference_root);
}

// ── (c) round-trip encode → decode → re-encode is byte-identical ──────────

#[test]
fn round_trip_encode_decode_reencode_is_byte_identical() {
    let entries = sample_entries();
    let manifest = Manifest::new(entries).unwrap();
    let wire = manifest.to_wire_string();

    let parsed = Manifest::from_wire_string(&wire).expect("valid wire string parses");
    assert_eq!(parsed, manifest);
    assert_eq!(parsed.to_wire_string(), wire);
    assert_eq!(parsed.root(), manifest.root());
}

#[test]
fn from_wire_string_recovers_exact_entries() {
    let entries = sample_entries();
    let manifest = Manifest::new(entries.clone()).unwrap();
    let wire = manifest.to_wire_string();
    let parsed = Manifest::from_wire_string(&wire).unwrap();
    assert_eq!(parsed.entries(), entries.as_slice());
    assert_eq!(parsed.len(), 3);
}

// ── (d) parser rejects malformed input ─────────────────────────────────────

#[test]
fn rejects_bad_version_prefix() {
    let digest = hex::encode(digest_of(b"x"));
    let bad = format!("v2,0:{digest}");
    assert!(Manifest::from_wire_string(&bad).is_err());
}

#[test]
fn rejects_uppercase_hex_digest() {
    let digest = hex::encode(digest_of(b"x")).to_uppercase();
    let bad = format!("v1,0:{digest}");
    assert!(Manifest::from_wire_string(&bad).is_err());
}

#[test]
fn rejects_leading_zero_offset() {
    let digest = hex::encode(digest_of(b"x"));
    let bad = format!("v1,01:{digest}");
    assert!(Manifest::from_wire_string(&bad).is_err());
}

#[test]
fn rejects_wrong_length_digest_too_short() {
    let digest = &hex::encode(digest_of(b"x"))[..63];
    let bad = format!("v1,0:{digest}");
    assert!(Manifest::from_wire_string(&bad).is_err());
}

#[test]
fn rejects_wrong_length_digest_too_long() {
    let digest = hex::encode(digest_of(b"x")) + "a";
    let bad = format!("v1,0:{digest}");
    assert!(Manifest::from_wire_string(&bad).is_err());
}

#[test]
fn rejects_non_ascending_offsets() {
    let d1 = hex::encode(digest_of(b"a"));
    let d2 = hex::encode(digest_of(b"b"));
    let bad = format!("v1,0:{d1},0:{d2}"); // duplicate offset, not strictly ascending
    assert!(Manifest::from_wire_string(&bad).is_err());

    let bad2 = format!("v1,10:{d1},5:{d2}"); // descending
    assert!(Manifest::from_wire_string(&bad2).is_err());
}

#[test]
fn rejects_first_offset_not_zero() {
    let digest = hex::encode(digest_of(b"x"));
    let bad = format!("v1,5:{digest}");
    assert!(Manifest::from_wire_string(&bad).is_err());
}

#[test]
fn rejects_empty_manifest_no_parts() {
    assert!(Manifest::from_wire_string("v1").is_err());
    assert!(Manifest::from_wire_string("").is_err());
}

#[test]
fn rejects_missing_colon_delimiter() {
    let digest = hex::encode(digest_of(b"x"));
    let bad = format!("v1,0{digest}");
    assert!(Manifest::from_wire_string(&bad).is_err());
}

#[test]
fn rejects_non_decimal_offset() {
    let digest = hex::encode(digest_of(b"x"));
    let bad = format!("v1,abc:{digest}");
    assert!(Manifest::from_wire_string(&bad).is_err());
}

#[test]
fn manifest_new_rejects_empty_entries() {
    assert!(Manifest::new(vec![]).is_err());
}

#[test]
fn manifest_new_rejects_first_offset_nonzero() {
    let entries = vec![ManifestEntry {
        offset: 1,
        digest: digest_of(b"x"),
    }];
    assert!(Manifest::new(entries).is_err());
}

#[test]
fn manifest_new_rejects_non_ascending_offsets() {
    let entries = vec![
        ManifestEntry {
            offset: 0,
            digest: digest_of(b"a"),
        },
        ManifestEntry {
            offset: 0,
            digest: digest_of(b"b"),
        },
    ];
    assert!(Manifest::new(entries).is_err());
}

#[test]
fn hash_mode_as_str_and_parse_round_trip() {
    assert_eq!(HashMode::WholeSha256.as_str(), "whole-sha256");
    assert_eq!(
        HashMode::MultipartCompositeSha256.as_str(),
        "multipart-composite-sha256"
    );
    assert_eq!(HashMode::parse("whole-sha256"), Some(HashMode::WholeSha256));
    assert_eq!(
        HashMode::parse("multipart-composite-sha256"),
        Some(HashMode::MultipartCompositeSha256)
    );
    assert_eq!(HashMode::parse("bogus"), None);
}
