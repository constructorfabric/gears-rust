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

#[test]
fn manifest_wire_string_is_exact_and_round_trips() {
    let entries = sample_entries();
    let manifest = Manifest::new(entries.clone()).expect("valid manifest");
    let [d0, d1, d2] = [0, 1, 2].map(|i| hex::encode(entries[i].digest));
    let wire = format!("v1,0:{d0},8388608:{d1},16777216:{d2}");
    assert_eq!(manifest.to_wire_string(), wire);

    // The root is the SHA-256 of the wire string, computed here without `Manifest::root()`.
    assert_eq!(manifest.root(), digest_of(wire.as_bytes()));

    let parsed = Manifest::from_wire_string(&wire).expect("valid wire string parses");
    assert_eq!(parsed, manifest);
    assert_eq!(parsed.entries(), entries.as_slice());
    assert_eq!(parsed.to_wire_string(), wire);
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
        let err = Manifest::from_wire_string(&wire).expect_err(name);
        assert!(
            matches!(err, DomainError::Validation { .. }),
            "{name}: expected Validation, got {err:?}"
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
        let err = Manifest::new(entries).expect_err(name);
        assert!(
            matches!(err, DomainError::Validation { .. }),
            "{name}: expected Validation, got {err:?}"
        );
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
