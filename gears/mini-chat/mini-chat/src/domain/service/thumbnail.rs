use image::{ImageDecoder as _, ImageReader};
use std::io::Cursor;
use toolkit_macros::domain_model;

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
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn test_config() -> ThumbnailConfig {
        ThumbnailConfig::default()
    }

    #[test]
    fn generates_thumbnail_from_valid_png() {
        // Create a minimal 2x2 red PNG in memory.
        let mut buf = Cursor::new(Vec::new());
        let img = image::RgbImage::from_pixel(2, 2, image::Rgb([255, 0, 0]));
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();

        let result = generate(&test_config(), buf.get_ref());
        assert!(result.is_some());
        let thumb = result.unwrap();
        assert!(thumb.width <= 128);
        assert!(thumb.height <= 128);
        assert!(!thumb.bytes.is_empty());

        // Verify output is valid WebP (RIFF....WEBP header).
        assert!(thumb.bytes.len() >= 12, "WebP output too short");
        assert_eq!(&thumb.bytes[0..4], b"RIFF", "missing RIFF header");
        assert_eq!(&thumb.bytes[8..12], b"WEBP", "missing WEBP signature");
    }

    #[test]
    fn returns_none_for_corrupt_data() {
        let result = generate(&test_config(), b"not an image");
        assert!(result.is_none());
    }

    #[test]
    fn returns_none_when_source_exceeds_decode_limit() {
        let mut cfg = test_config();
        cfg.max_decode_bytes = 10; // absurdly small
        let mut buf = Cursor::new(Vec::new());
        let img = image::RgbImage::from_pixel(2, 2, image::Rgb([0, 0, 0]));
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();

        let result = generate(&cfg, buf.get_ref());
        assert!(result.is_none());
    }

    #[test]
    fn respects_max_pixels_limit() {
        let mut cfg = test_config();
        cfg.max_pixels = 1; // only 1 pixel allowed
        let mut buf = Cursor::new(Vec::new());
        let img = image::RgbImage::from_pixel(2, 2, image::Rgb([0, 0, 0]));
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();

        let result = generate(&cfg, buf.get_ref());
        assert!(result.is_none());
    }

    #[test]
    fn returns_none_when_encoded_output_exceeds_max_bytes() {
        let mut cfg = test_config();
        cfg.max_bytes = 1; // impossibly small — any valid WebP will exceed this
        let mut buf = Cursor::new(Vec::new());
        let img = image::RgbImage::from_pixel(2, 2, image::Rgb([0, 0, 0]));
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();

        let result = generate(&cfg, buf.get_ref());
        assert!(result.is_none());
    }

    #[test]
    fn resizes_large_image() {
        let mut buf = Cursor::new(Vec::new());
        let img = image::RgbImage::from_pixel(1000, 500, image::Rgb([0, 128, 255]));
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();

        let result = generate(&test_config(), buf.get_ref());
        assert!(result.is_some());
        let thumb = result.unwrap();
        // 1000x500 → fit inside 128x128 → 128x64
        assert_eq!(thumb.width, 128);
        assert_eq!(thumb.height, 64);
    }
}
