#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io;

use bytes::Bytes;
use futures::StreamExt;
use futures::stream::{self, BoxStream};

use super::verify_stream;
use crate::infra::content::hash;
use crate::infra::content::hash_mode::{HashMode, Manifest, ManifestEntry};

fn ok_stream(chunks: Vec<Vec<u8>>) -> BoxStream<'static, io::Result<Bytes>> {
    Box::pin(stream::iter(chunks.into_iter().map(|c| Ok(Bytes::from(c)))))
}

/// Drains `s` fully, returning the concatenated `Ok` bytes and, if the stream
/// ends on an `Err`, that error's kind.
async fn drain(mut s: BoxStream<'static, io::Result<Bytes>>) -> (Vec<u8>, Option<io::ErrorKind>) {
    let mut collected = Vec::new();
    let mut err_kind = None;
    while let Some(item) = s.next().await {
        match item {
            Ok(chunk) => collected.extend_from_slice(&chunk),
            Err(e) => {
                err_kind = Some(e.kind());
                assert!(
                    s.next().await.is_none(),
                    "the stream must not yield anything after its terminal error"
                );
                break;
            }
        }
    }
    (collected, err_kind)
}

/// Build a `multipart-composite-sha256` manifest + root from a list of part
/// byte slices, mirroring how a real multipart completion builds one.
fn build_composite(parts: &[&[u8]]) -> (Manifest, Vec<u8>) {
    let mut offset = 0u64;
    let entries: Vec<ManifestEntry> = parts
        .iter()
        .map(|p| {
            let entry = ManifestEntry {
                offset,
                digest: hash::digest_to_array(hash::sha256(p)),
            };
            offset += p.len() as u64;
            entry
        })
        .collect();
    let manifest = Manifest::new(entries).expect("valid manifest");
    let root = manifest.root().to_vec();
    (manifest, root)
}

// ── whole-sha256 mode ────────────────────────────────────────────────────

#[tokio::test]
async fn whole_mode_matching_content_verifies_ok() {
    let content = b"the quick brown fox jumps over the lazy dog".to_vec();
    let hash_value = hash::sha256(&content);
    let inner = ok_stream(vec![
        content[0..10].to_vec(),
        content[10..20].to_vec(),
        content[20..].to_vec(),
    ]);
    let (wrapped, slot) = verify_stream(
        inner,
        content.len() as u64,
        HashMode::WholeSha256,
        hash_value,
        None,
    )
    .expect("valid whole-mode construction");
    let (collected, err) = drain(wrapped).await;
    assert_eq!(collected, content);
    assert_eq!(err, None);
    assert!(
        slot.lock()
            .unwrap()
            .take()
            .expect("verdict published")
            .is_ok(),
        "matching content must verify"
    );
}

#[tokio::test]
async fn whole_mode_tampered_content_fails_verification() {
    let content = b"the quick brown fox jumps over the lazy dog".to_vec();
    let hash_value = hash::sha256(&content);
    let mut tampered = content.clone();
    tampered[5] ^= 0xff;
    let inner = ok_stream(vec![tampered.clone()]);
    let (wrapped, slot) = verify_stream(
        inner,
        tampered.len() as u64,
        HashMode::WholeSha256,
        hash_value,
        None,
    )
    .expect("valid whole-mode construction");
    let (collected, err) = drain(wrapped).await;
    // The wrapper still forwards every byte -- the destination write must
    // complete so there is something for the caller to clean up.
    assert_eq!(collected, tampered);
    assert_eq!(err, None);
    let verdict = slot.lock().unwrap().take().expect("verdict published");
    assert!(
        matches!(
            verdict,
            Err(crate::domain::error::DomainError::HashMismatch { .. })
        ),
        "tampered content must fail verification: {verdict:?}"
    );
}

#[tokio::test]
async fn whole_mode_empty_object_verifies_against_empty_hash() {
    let hash_value = hash::sha256(b"");
    let inner = ok_stream(vec![]);
    let (wrapped, slot) = verify_stream(inner, 0, HashMode::WholeSha256, hash_value, None)
        .expect("valid whole-mode construction");
    let (collected, err) = drain(wrapped).await;
    assert!(collected.is_empty());
    assert_eq!(err, None);
    assert!(
        slot.lock()
            .unwrap()
            .take()
            .expect("verdict published")
            .is_ok()
    );
}

#[tokio::test]
async fn whole_mode_shorter_than_declared_size_fails_with_length_mismatch() {
    let content = b"short".to_vec();
    let hash_value = hash::sha256(&content);
    let inner = ok_stream(vec![content.clone()]);
    // Declare a length longer than what the stream actually yields.
    let (wrapped, slot) = verify_stream(
        inner,
        (content.len() + 5) as u64,
        HashMode::WholeSha256,
        hash_value,
        None,
    )
    .expect("valid whole-mode construction");
    let (_collected, err) = drain(wrapped).await;
    assert_eq!(
        err, None,
        "the wrapper itself never errors the stream on a length mismatch"
    );
    let verdict = slot.lock().unwrap().take().expect("verdict published");
    assert!(
        matches!(
            verdict,
            Err(crate::domain::error::DomainError::HashMismatch { .. })
        ),
        "an undersized stream must fail verification: {verdict:?}"
    );
}

#[tokio::test]
async fn whole_mode_longer_than_declared_size_fails_with_length_mismatch() {
    let content = b"this stream yields more bytes than declared".to_vec();
    let declared_len = 10u64; // much shorter than the real content
    let hash_value = hash::sha256(&content[..10]);
    let inner = ok_stream(vec![content.clone()]);
    let (wrapped, slot) =
        verify_stream(inner, declared_len, HashMode::WholeSha256, hash_value, None)
            .expect("valid whole-mode construction");
    let (collected, err) = drain(wrapped).await;
    // All bytes are still forwarded -- the wrapper never truncates.
    assert_eq!(collected, content);
    assert_eq!(err, None);
    let verdict = slot.lock().unwrap().take().expect("verdict published");
    assert!(
        matches!(
            verdict,
            Err(crate::domain::error::DomainError::HashMismatch { .. })
        ),
        "an oversized stream must fail verification: {verdict:?}"
    );
}

#[tokio::test]
async fn whole_mode_rejects_a_manifest() {
    let err = verify_stream(
        ok_stream(vec![]),
        0,
        HashMode::WholeSha256,
        hash::sha256(b""),
        Some(
            Manifest::new(vec![ManifestEntry {
                offset: 0,
                digest: [0u8; 32],
            }])
            .unwrap(),
        ),
    )
    .map(|_| ())
    .expect_err("whole-sha256 with a manifest must be rejected");
    assert!(matches!(
        err,
        crate::domain::error::DomainError::Validation { .. }
    ));
}

// ── multipart-composite-sha256 mode ─────────────────────────────────────

#[tokio::test]
async fn composite_mode_matching_parts_verify_ok_with_chunk_boundaries_unaligned_to_parts() {
    let part1 = vec![b'a'; 100];
    let part2 = vec![b'b'; 200];
    let part3 = vec![b'c'; 50];
    let (manifest, root) = build_composite(&[&part1, &part2, &part3]);
    let mut whole = Vec::new();
    whole.extend_from_slice(&part1);
    whole.extend_from_slice(&part2);
    whole.extend_from_slice(&part3);

    // Chunk boundaries deliberately do NOT line up with part boundaries: the
    // first chunk straddles the part1/part2 boundary, and a later chunk
    // straddles part2/part3.
    let chunks = vec![
        whole[0..150].to_vec(),   // ends 50 bytes into part2
        whole[150..340].to_vec(), // ends 40 bytes into part3
        whole[340..].to_vec(),
    ];
    let inner = ok_stream(chunks);
    let (wrapped, slot) = verify_stream(
        inner,
        whole.len() as u64,
        HashMode::MultipartCompositeSha256,
        root,
        Some(manifest),
    )
    .expect("valid composite-mode construction");
    let (collected, err) = drain(wrapped).await;
    assert_eq!(collected, whole);
    assert_eq!(err, None);
    assert!(
        slot.lock()
            .unwrap()
            .take()
            .expect("verdict published")
            .is_ok(),
        "matching multipart content must verify, even with chunk boundaries crossing part boundaries"
    );
}

#[tokio::test]
async fn composite_mode_boundary_falls_mid_chunk_still_hashes_each_part_correctly() {
    let part1 = vec![1u8; 4];
    let part2 = vec![2u8; 4];
    let (manifest, root) = build_composite(&[&part1, &part2]);
    let mut whole = Vec::new();
    whole.extend_from_slice(&part1);
    whole.extend_from_slice(&part2);

    // A single chunk spanning the entire object -- the part boundary (at
    // offset 4) falls squarely in the middle of this one chunk.
    let inner = ok_stream(vec![whole.clone()]);
    let (wrapped, slot) = verify_stream(
        inner,
        whole.len() as u64,
        HashMode::MultipartCompositeSha256,
        root,
        Some(manifest),
    )
    .expect("valid composite-mode construction");
    let (collected, err) = drain(wrapped).await;
    assert_eq!(collected, whole);
    assert_eq!(err, None);
    assert!(
        slot.lock()
            .unwrap()
            .take()
            .expect("verdict published")
            .is_ok(),
        "a part boundary landing mid-chunk must still be hashed correctly per part"
    );
}

#[tokio::test]
async fn composite_mode_tampered_part_fails_verification() {
    let part1 = vec![b'x'; 30];
    let part2 = vec![b'y'; 30];
    let (manifest, root) = build_composite(&[&part1, &part2]);
    let mut tampered_part2 = part2.clone();
    tampered_part2[0] ^= 0xff;
    let mut whole = Vec::new();
    whole.extend_from_slice(&part1);
    whole.extend_from_slice(&tampered_part2);

    let inner = ok_stream(vec![whole.clone()]);
    let (wrapped, slot) = verify_stream(
        inner,
        whole.len() as u64,
        HashMode::MultipartCompositeSha256,
        root,
        Some(manifest),
    )
    .expect("valid composite-mode construction");
    let (_collected, err) = drain(wrapped).await;
    assert_eq!(err, None);
    let verdict = slot.lock().unwrap().take().expect("verdict published");
    assert!(
        matches!(
            verdict,
            Err(crate::domain::error::DomainError::HashMismatch { .. })
        ),
        "a tampered part must fail verification: {verdict:?}"
    );
}

#[tokio::test]
async fn composite_mode_requires_a_manifest() {
    let err = verify_stream(
        ok_stream(vec![]),
        0,
        HashMode::MultipartCompositeSha256,
        vec![0u8; 32],
        None,
    )
    .map(|_| ())
    .expect_err("multipart-composite-sha256 without a manifest must be rejected");
    assert!(matches!(
        err,
        crate::domain::error::DomainError::Validation { .. }
    ));
}

// ── inner stream errors ──────────────────────────────────────────────────

#[tokio::test]
async fn inner_read_error_passes_through_and_never_publishes_a_verdict() {
    let inner: BoxStream<'static, io::Result<Bytes>> = Box::pin(stream::iter(vec![
        Ok(Bytes::from_static(b"partial")),
        Err(io::Error::new(io::ErrorKind::TimedOut, "boom")),
    ]));
    let (wrapped, slot) = verify_stream(
        inner,
        100,
        HashMode::WholeSha256,
        hash::sha256(b"irrelevant"),
        None,
    )
    .expect("valid whole-mode construction");
    let (collected, err) = drain(wrapped).await;
    assert_eq!(collected, b"partial");
    assert_eq!(err, Some(io::ErrorKind::TimedOut));
    assert!(
        slot.lock().unwrap().is_none(),
        "a stream that errors before completion must never publish a verdict"
    );
}
