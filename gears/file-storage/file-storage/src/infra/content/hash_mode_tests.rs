//! Wire-format tests for `Manifest` (ADR-0006): the grammar must be byte-for-byte unambiguous.

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

/// Hand-written expected wire string for `sample_entries`, independent of `Manifest`,
/// so the test cannot pass by tautology.
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

#[test]
fn to_wire_string_matches_hand_written_expected_string_exactly() {
    let entries = sample_entries();
    let manifest = Manifest::new(entries.clone()).expect("valid manifest");
    assert_eq!(manifest.to_wire_string(), expected_wire_string(&entries));

    let d0 = hex::encode(entries[0].digest);
    let d1 = hex::encode(entries[1].digest);
    let d2 = hex::encode(entries[2].digest);
    assert_eq!(
        manifest.to_wire_string(),
        format!("v1,0:{d0},8388608:{d1},16777216:{d2}")
    );
}

#[test]
fn to_wire_string_single_part_minimal_shape() {
    let digest = digest_of(b"only-part");
    let manifest = Manifest::new(vec![ManifestEntry { offset: 0, digest }]).unwrap();
    assert_eq!(
        manifest.to_wire_string(),
        format!("v1,0:{}", hex::encode(digest))
    );
}

#[test]
fn root_matches_independently_computed_reference_sha256() {
    let entries = sample_entries();
    let manifest = Manifest::new(entries.clone()).unwrap();
    let wire = expected_wire_string(&entries);

    // Reference hash computed with a fresh `Sha256`, not through `Manifest::root()`.
    let reference_root: [u8; 32] = {
        let mut hasher = Sha256::new();
        hasher.update(wire.as_bytes());
        hasher.finalize().into()
    };

    assert_eq!(manifest.root(), reference_root);
}

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

#[test]
fn from_wire_string_rejects_malformed_input() {
    let d = hex::encode(digest_of(b"x"));
    let d2 = hex::encode(digest_of(b"y"));
    let cases: Vec<(&str, String)> = vec![
        ("bad version prefix", format!("v2,0:{d}")),
        ("uppercase hex digest", format!("v1,0:{}", d.to_uppercase())),
        ("leading-zero offset", format!("v1,01:{d}")),
        ("digest too short", format!("v1,0:{}", &d[..63])),
        ("digest too long", format!("v1,0:{d}a")),
        ("duplicate offset", format!("v1,0:{d},0:{d2}")),
        ("descending offsets", format!("v1,10:{d},5:{d2}")),
        ("first offset not zero", format!("v1,5:{d}")),
        ("no parts", "v1".to_owned()),
        ("empty string", String::new()),
        ("missing colon delimiter", format!("v1,0{d}")),
        ("non-decimal offset", format!("v1,abc:{d}")),
    ];
    for (name, wire) in cases {
        assert!(
            Manifest::from_wire_string(&wire).is_err(),
            "{name} must be rejected: {wire}"
        );
    }
}

#[test]
fn manifest_new_rejects_invalid_entries() {
    let entry = |offset: u64, seed: &[u8]| ManifestEntry {
        offset,
        digest: digest_of(seed),
    };
    let cases: Vec<(&str, Vec<ManifestEntry>)> = vec![
        ("empty entries", vec![]),
        ("first offset non-zero", vec![entry(1, b"x")]),
        ("duplicate offsets", vec![entry(0, b"a"), entry(0, b"b")]),
    ];
    for (name, entries) in cases {
        assert!(Manifest::new(entries).is_err(), "{name} must be rejected");
    }
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
