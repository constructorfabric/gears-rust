//! Streaming, mode-aware content-hash verification
//! (ADR-0006 `cpt-cf-file-storage-algo-content-hash-modes-verify`) for
//! [`migrate_backend`](crate::domain::service::FileService::migrate_backend).
//!
//! Wraps a `BoxStream<io::Result<Bytes>>` so every chunk it yields is
//! forwarded to the consumer completely unchanged, while being hashed
//! incrementally — mode-aware, exactly like `Store::verify_content_hash`
//! hashes a fully-buffered blob — and the running total byte count is
//! tracked against the object's declared length. Never buffers more than the
//! single chunk currently in flight: the running hash state (a `Sha256`
//! accumulator plus one finished digest per part) is bounded by the number of
//! parts, not by the object's size.
//!
//! There is no way to know whether the hash matches before every byte has
//! been read, so the verdict is only available once the wrapped stream has
//! been fully drained (polled to a terminal `None`) by its consumer — e.g.
//! [`migrate_backend`](crate::domain::service::FileService::migrate_backend)'s
//! destination write. This is therefore a "write first, check second"
//! pattern: the caller must read the [`VerifySlot`] only after the stream's
//! consumer has finished with it, and must treat an unpopulated slot (the
//! consumer stopped early, or an upstream read error cut the stream short)
//! exactly like a verification failure — never assume success from silence.
//!
//! Both content-hash modes share the same per-chunk mechanics: whole-object
//! mode is composite mode's degenerate one-span case (`[0, expected_len)`),
//! so this module hashes one "current span" at a time regardless of mode,
//! splitting an incoming chunk at a span boundary when one falls inside it
//! (a boundary may land anywhere within a chunk, not just at its edges — one
//! chunk may even cross more than one boundary for small parts). Whole-object
//! mode's single accumulated digest is compared directly to the version's
//! `hash_value`; `multipart-composite-sha256`'s per-part digests are compared
//! to the manifest's recorded digests and then rebuilt into a
//! [`Manifest`] to recompute `root` via [`Manifest::new`]/[`Manifest::root`]
//! — the exact functions `complete_multipart`/`Store::verify_multipart_composite`
//! use, so the manifest wire-encoding is never hand-rolled a second time here.

use std::io;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;

use crate::domain::error::DomainError;
use crate::infra::content::hash::{Hasher, digest_to_array};
use crate::infra::content::hash_mode::{HashMode, Manifest, ManifestEntry};

/// Where [`verify_stream`] publishes its verdict once its returned stream has
/// been fully drained by its consumer. Stays `None` if the stream is never
/// drained to completion (the consumer stops early, or an upstream read
/// error ends the stream first) — a caller must treat an unpopulated slot the
/// same as a verification failure.
pub type VerifySlot = Arc<Mutex<Option<Result<(), DomainError>>>>;

/// [`verify_stream`]'s return value: the wrapped, forwarding stream paired
/// with the [`VerifySlot`] it will eventually publish into.
pub type VerifiedStream = (BoxStream<'static, io::Result<Bytes>>, VerifySlot);

/// `unfold` state driving [`verify_stream`]'s wrapped stream.
enum VerifyState {
    Reading {
        inner: BoxStream<'static, io::Result<Bytes>>,
        total_seen: u64,
        span_idx: usize,
        hasher: Hasher,
        part_digests: Vec<[u8; 32]>,
    },
    Done,
}

/// One `[start, end)` byte span of the object hashed as a single unit.
/// Whole-object mode has exactly one span covering the entire object;
/// composite mode has one span per manifest part.
struct Span {
    end: u64,
}

/// What to compare the finished per-span digests against once every span has
/// been hashed.
enum Spec {
    /// Compare the (single) finished digest directly to `hash_value`.
    Whole,
    /// Compare each finished digest to its manifest entry's recorded digest,
    /// then rebuild the manifest from the recomputed digests and compare its
    /// `root()` to `hash_value`.
    Composite { entries: Vec<ManifestEntry> },
}

/// Validate `hash_mode`/`manifest` agree (mirrors
/// `Store::verify_content_hash`'s own mode check) and derive the byte spans
/// to hash separately, one call up front rather than re-deriving them per
/// chunk.
fn build_spec(
    expected_len: u64,
    hash_mode: HashMode,
    manifest: Option<Manifest>,
    hash_value: &[u8],
) -> Result<(Vec<Span>, Spec), DomainError> {
    match hash_mode {
        HashMode::WholeSha256 => {
            if manifest.is_some() {
                return Err(DomainError::validation(
                    "manifest",
                    "whole-sha256 versions carry no manifest",
                ));
            }
            Ok((vec![Span { end: expected_len }], Spec::Whole))
        }
        HashMode::MultipartCompositeSha256 => {
            let manifest = manifest.ok_or_else(|| {
                DomainError::validation(
                    "manifest",
                    "multipart-composite-sha256 verification requires the stored manifest",
                )
            })?;
            let entries = manifest.entries().to_vec();
            let mut spans = Vec::with_capacity(entries.len());
            for (i, entry) in entries.iter().enumerate() {
                let start = entry.offset;
                let end = entries.get(i + 1).map_or(expected_len, |next| next.offset);
                if start > end || end > expected_len {
                    return Err(DomainError::hash_mismatch(
                        hex::encode(hash_value),
                        format!(
                            "manifest offset {start} out of range for a {expected_len} byte(s) object"
                        ),
                    ));
                }
                spans.push(Span { end });
            }
            Ok((spans, Spec::Composite { entries }))
        }
    }
}

/// Feed `chunk`'s bytes into whichever span(s) they fall into, advancing
/// `span_idx`/`hasher`/`part_digests` and finalizing a span's digest the
/// instant `total_seen` reaches its end — possibly more than once per chunk,
/// if a chunk happens to cross more than one span boundary (e.g. several tiny
/// parts). Bytes arriving after every span is already finished are still
/// counted into `total_seen` (so an oversized stream is still caught by the
/// final length check) but are never hashed into anything.
///
/// `chunk` is sliced from the front via [`Bytes::split_to`] (a cheap,
/// refcounted slice — never copies), so this never holds more than the
/// current chunk's bytes at once.
fn process_chunk(
    mut chunk: Bytes,
    spans: &[Span],
    span_idx: &mut usize,
    total_seen: &mut u64,
    hasher: &mut Hasher,
    part_digests: &mut Vec<[u8; 32]>,
) {
    while !chunk.is_empty() {
        let Some(span) = spans.get(*span_idx) else {
            // Every span is already finished -- these are overage bytes;
            // still counted for the final length check, never hashed.
            *total_seen += chunk.len() as u64;
            return;
        };
        let remaining_in_span = span.end.saturating_sub(*total_seen);
        let take = usize::try_from(remaining_in_span)
            .unwrap_or(usize::MAX)
            .min(chunk.len());
        if take > 0 {
            let head = chunk.split_to(take);
            hasher.update(&head);
            *total_seen += take as u64;
        }
        if *total_seen == span.end {
            let digest = digest_to_array(std::mem::take(hasher).finalize());
            part_digests.push(digest);
            *span_idx += 1;
        }
    }
}

/// Compare the finished per-span digests against `spec`, mode-aware.
fn compare(spec: &Spec, hash_value: &[u8], part_digests: Vec<[u8; 32]>) -> Result<(), DomainError> {
    match spec {
        Spec::Whole => {
            let digest = part_digests
                .into_iter()
                .next()
                .unwrap_or_else(|| digest_to_array(Hasher::new().finalize()));
            if digest.as_slice() != hash_value {
                return Err(DomainError::hash_mismatch(
                    hex::encode(hash_value),
                    hex::encode(digest),
                ));
            }
            Ok(())
        }
        Spec::Composite { entries } => {
            for (entry, digest) in entries.iter().zip(part_digests.iter()) {
                if *digest != entry.digest {
                    return Err(DomainError::hash_mismatch(
                        hex::encode(entry.digest),
                        format!(
                            "recomputed part digest at offset {}: {}",
                            entry.offset,
                            hex::encode(digest)
                        ),
                    ));
                }
            }
            let rebuilt: Vec<ManifestEntry> = entries
                .iter()
                .zip(part_digests.iter())
                .map(|(e, d)| ManifestEntry {
                    offset: e.offset,
                    digest: *d,
                })
                .collect();
            let root = Manifest::new(rebuilt)?.root();
            if root.as_slice() != hash_value {
                return Err(DomainError::hash_mismatch(
                    hex::encode(hash_value),
                    hex::encode(root),
                ));
            }
            Ok(())
        }
    }
}

/// Wrap `inner` so every chunk it yields is forwarded unchanged while its
/// content hash is verified incrementally (mode-aware, ADR-0006) and its
/// total length is checked against `expected_len`. Returns the wrapped stream
/// plus a [`VerifySlot`] readable once the caller has finished draining it —
/// see the module doc comment for the full contract.
///
/// `hash_value`/`manifest` mirror `Store::verify_content_hash`'s own
/// parameters: `manifest` must be `Some` iff `hash_mode` is
/// `MultipartCompositeSha256`.
///
/// # Errors
/// Returns an `Err` immediately (before wrapping `inner`) if `hash_mode`/
/// `manifest` disagree, or if a composite manifest's part offsets don't fit
/// within `expected_len`.
pub fn verify_stream(
    inner: BoxStream<'static, io::Result<Bytes>>,
    expected_len: u64,
    hash_mode: HashMode,
    hash_value: Vec<u8>,
    manifest: Option<Manifest>,
) -> Result<VerifiedStream, DomainError> {
    let (spans, spec) = build_spec(expected_len, hash_mode, manifest, &hash_value)?;
    let spans = Arc::new(spans);
    let spec = Arc::new(spec);
    let hash_value = Arc::new(hash_value);

    let slot: VerifySlot = Arc::new(Mutex::new(None));
    let out_slot = Arc::clone(&slot);

    let initial = VerifyState::Reading {
        inner,
        total_seen: 0,
        span_idx: 0,
        hasher: Hasher::new(),
        part_digests: Vec::with_capacity(spans.len()),
    };

    let stream = futures::stream::unfold(initial, move |state| {
        let slot = Arc::clone(&slot);
        let spans = Arc::clone(&spans);
        let spec = Arc::clone(&spec);
        let hash_value = Arc::clone(&hash_value);
        async move {
            match state {
                VerifyState::Done => None,
                VerifyState::Reading {
                    mut inner,
                    mut total_seen,
                    mut span_idx,
                    mut hasher,
                    mut part_digests,
                } => match inner.next().await {
                    None => {
                        let verdict = if total_seen == expected_len {
                            compare(&spec, &hash_value, part_digests)
                        } else {
                            Err(DomainError::hash_mismatch(
                                format!("{expected_len} byte(s) (declared version size)"),
                                format!(
                                    "{total_seen} byte(s) actually read from the source backend"
                                ),
                            ))
                        };
                        if let Ok(mut s) = slot.lock() {
                            *s = Some(verdict);
                        }
                        None
                    }
                    Some(Err(e)) => Some((Err(e), VerifyState::Done)),
                    Some(Ok(chunk)) => {
                        let forward = chunk.clone();
                        process_chunk(
                            chunk,
                            &spans,
                            &mut span_idx,
                            &mut total_seen,
                            &mut hasher,
                            &mut part_digests,
                        );
                        Some((
                            Ok(forward),
                            VerifyState::Reading {
                                inner,
                                total_seen,
                                span_idx,
                                hasher,
                                part_digests,
                            },
                        ))
                    }
                },
            }
        }
    });

    Ok((Box::pin(stream), out_slot))
}

#[cfg(test)]
#[path = "stream_verify_tests.rs"]
mod stream_verify_tests;
