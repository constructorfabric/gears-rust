use image::{ImageDecoder as _, ImageReader};
use modkit_macros::domain_model;
use std::io::Cursor;

use crate::config::ThumbnailConfig;

/// Generated thumbnail data ready to be persisted.
#[domain_model]
pub struct Thumbnail {
    /// WebP-encoded thumbnail bytes.
    pub bytes: Vec<u8>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Best-effort thumbnail generation from raw image bytes.
///
/// Returns `None` (rather than an error) when:
/// - the source image exceeds the configured pixel/decode-byte limits,
/// - the image cannot be decoded (unsupported sub-format, corrupt data),
/// - the produced WebP exceeds `max_bytes`.
///
/// This matches DESIGN.md: *"Thumbnail failure does not set `error_code`
/// on the attachment; the attachment transitions to `ready` with
/// `img_thumbnail = null`."*
#[allow(clippy::cognitive_complexity)]
pub fn generate(cfg: &ThumbnailConfig, raw: &[u8]) -> Option<Thumbnail> {
    // Pre-screen: reject oversized compressed payloads before decoding.
    // max_decode_bytes is the memory budget for the decoded bitmap; using it as
    // an input-size guard too is intentionally conservative — a compressed file
    // larger than the decoded budget will always exceed it once decoded.
    if raw.len() > cfg.max_decode_bytes {
        tracing::debug!(
            raw_len = raw.len(),
            max = cfg.max_decode_bytes,
            "thumbnail skipped: compressed input exceeds max_decode_bytes"
        );
        return None;
    }

    let reader = ImageReader::new(Cursor::new(raw))
        .with_guessed_format()
        .ok()?;

    // Check pixel dimensions before full decode (cheap metadata read).
    let (w, h) = match reader.into_dimensions() {
        Ok(dims) => dims,
        Err(e) => {
            tracing::debug!(error = %e, "thumbnail skipped: cannot read dimensions");
            return None;
        }
    };
    if u64::from(w) * u64::from(h) > cfg.max_pixels {
        tracing::debug!(
            pixels = u64::from(w) * u64::from(h),
            max = cfg.max_pixels,
            "thumbnail skipped: source exceeds max_pixels"
        );
        return None;
    }

    // Deterministic pre-decode guard: reject images whose worst-case decoded
    // size (RGBA = 4 bytes per pixel) would exceed the memory budget. This
    // avoids relying solely on image::Limits::max_alloc inside the decoder.
    let decoded_estimate = u64::from(w) * u64::from(h) * 4;
    if decoded_estimate > cfg.max_decode_bytes as u64 {
        tracing::debug!(
            decoded_estimate,
            max = cfg.max_decode_bytes,
            "thumbnail skipped: estimated decoded size exceeds max_decode_bytes"
        );
        return None;
    }

    // Re-create reader and apply memory limit for the full decode.
    let reader = ImageReader::new(Cursor::new(raw))
        .with_guessed_format()
        .ok()?;
    let mut reader = reader;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(cfg.max_decode_bytes as u64);
    reader.limits(limits);

    let mut decoder = match reader.into_decoder() {
        Ok(d) => d,
        Err(e) => {
            tracing::debug!(error = %e, "thumbnail skipped: decoder creation failed");
            return None;
        }
    };

    // Read EXIF orientation before decoding pixels (best-effort; default to
    // no-op orientation on unsupported formats).
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);

    let mut img: image::DynamicImage = match image::DynamicImage::from_decoder(decoder) {
        Ok(img) => img,
        Err(e) => {
            tracing::debug!(error = %e, "thumbnail skipped: decode failed");
            return None;
        }
    };

    // Apply EXIF orientation so the thumbnail matches what the user sees.
    img.apply_orientation(orientation);

    // Resize to fit inside target dimensions, preserving aspect ratio.
    let resized = img.thumbnail(cfg.width, cfg.height);

    let tw = resized.width();
    let th = resized.height();

    // Encode as WebP.
    let mut buf = Cursor::new(Vec::new());
    if let Err(e) = resized.write_to(&mut buf, image::ImageFormat::WebP) {
        tracing::debug!(error = %e, "thumbnail skipped: WebP encode failed");
        return None;
    }
    let webp_bytes = buf.into_inner();

    // Enforce size limit on the encoded output.
    if webp_bytes.len() > cfg.max_bytes {
        tracing::debug!(
            encoded_len = webp_bytes.len(),
            max = cfg.max_bytes,
            "thumbnail skipped: encoded size exceeds max_bytes"
        );
        return None;
    }

    Some(Thumbnail {
        bytes: webp_bytes,
        width: tw,
        height: th,
    })
}
#[cfg(test)]
#[path = "thumbnail_tests.rs"]
mod tests;
