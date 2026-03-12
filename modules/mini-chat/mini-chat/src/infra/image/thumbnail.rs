use std::io::Cursor;

use image::{DynamicImage, ImageFormat, ImageReader, Limits};
use thiserror::Error;

/// Configuration for thumbnail generation.
#[derive(Debug, Clone)]
pub struct ThumbnailConfig {
    pub width: u32,
    pub height: u32,
    pub max_bytes: usize,
    pub max_pixels: u64,
    pub max_decode_bytes: usize,
}

/// Result of successful thumbnail generation.
#[derive(Debug)]
pub struct ThumbnailResult {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Error)]
pub enum ThumbnailError {
    #[error("Image exceeds max pixels ({pixels} > {max})")]
    TooManyPixels { pixels: u64, max: u64 },

    #[error("Thumbnail exceeds max bytes ({size} > {max})")]
    TooLarge { size: usize, max: usize },

    #[error("Image decode failed: {0}")]
    DecodeFailed(String),

    #[error("WebP encode failed: {0}")]
    EncodeFailed(String),

    #[error("Unsupported image format: {0}")]
    UnsupportedFormat(String),
}

/// Generate a thumbnail from image bytes.
///
/// Steps:
/// 1. Read image dimensions from header — skip if w*h > `max_pixels` (pre-screening heuristic)
/// 2. Set decoder limits (`max_decode_bytes` — security boundary against pixel-bomb attacks)
/// 3. Decode the image
/// 4. Resize to fit within configured dimensions preserving aspect ratio
/// 5. Reject if decoded (RGBA) size > `max_bytes`, then encode as WebP
pub fn generate_thumbnail(
    bytes: &[u8],
    content_type: &str,
    config: &ThumbnailConfig,
) -> Result<ThumbnailResult, ThumbnailError> {
    let format = match content_type {
        "image/png" => ImageFormat::Png,
        "image/jpeg" => ImageFormat::Jpeg,
        "image/webp" => ImageFormat::WebP,
        other => return Err(ThumbnailError::UnsupportedFormat(other.to_owned())),
    };

    let cursor = Cursor::new(bytes);
    let mut reader = ImageReader::new(cursor);
    reader.set_format(format);

    // Set decoder limits — SECURITY BOUNDARY against pixel-bomb attacks
    let mut limits = Limits::default();
    limits.max_alloc = Some(config.max_decode_bytes as u64);
    reader.limits(limits);

    // Pre-screen: fast-reject heuristic based on header-reported dimensions.
    // NOTE: this only reads the image header — a crafted file could report small
    // dimensions but decompress to enormous size (pixel-bomb via progressive JPEG
    // or interlaced PNG). The `max_alloc` limit set below is the actual security
    // boundary that caps decoder memory allocation.
    if let Ok((w, h)) = reader.into_dimensions() {
        let pixels = u64::from(w) * u64::from(h);
        if pixels > config.max_pixels {
            return Err(ThumbnailError::TooManyPixels {
                pixels,
                max: config.max_pixels,
            });
        }
    }

    // Re-create reader for actual decoding (`into_dimensions` consumed the previous one)
    let cursor = Cursor::new(bytes);
    let mut reader = ImageReader::new(cursor);
    reader.set_format(format);
    let mut limits = Limits::default();
    limits.max_alloc = Some(config.max_decode_bytes as u64);
    reader.limits(limits);

    let img: DynamicImage = reader
        .decode()
        .map_err(|e| ThumbnailError::DecodeFailed(e.to_string()))?;

    // Resize preserving aspect ratio (fit within config dimensions)
    let resized = img.thumbnail(config.width, config.height);

    // Guard: reject if the decoded thumbnail exceeds max_bytes (DESIGN: max
    // *decoded* size, not encoded). RGBA8 = 4 bytes per pixel.
    let decoded_size = (resized.width() as usize) * (resized.height() as usize) * 4;
    if decoded_size > config.max_bytes {
        return Err(ThumbnailError::TooLarge {
            size: decoded_size,
            max: config.max_bytes,
        });
    }

    // Encode as WebP
    let mut webp_buf = Vec::new();
    let mut cursor = Cursor::new(&mut webp_buf);
    resized
        .write_to(&mut cursor, ImageFormat::WebP)
        .map_err(|e| ThumbnailError::EncodeFailed(e.to_string()))?;

    Ok(ThumbnailResult {
        data: webp_buf,
        width: resized.width(),
        height: resized.height(),
    })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::{DynamicImage, ImageFormat, RgbaImage};

    use super::{ThumbnailConfig, ThumbnailError, generate_thumbnail};

    fn default_config() -> ThumbnailConfig {
        ThumbnailConfig {
            width: 128,
            height: 128,
            max_bytes: 131_072,
            max_pixels: 100_000_000,
            max_decode_bytes: 33_554_432,
        }
    }

    /// Encode a solid-color image as PNG/JPEG/WebP bytes.
    fn make_image_bytes(w: u32, h: u32, format: ImageFormat) -> Vec<u8> {
        let img = DynamicImage::ImageRgba8(RgbaImage::from_pixel(
            w,
            h,
            image::Rgba([100, 150, 200, 255]),
        ));
        let mut buf = Vec::new();
        let mut cursor = Cursor::new(&mut buf);
        img.write_to(&mut cursor, format).unwrap();
        buf
    }

    // ── Happy paths ──

    #[test]
    fn jpeg_landscape_produces_webp_with_correct_aspect_ratio() {
        let bytes = make_image_bytes(400, 300, ImageFormat::Jpeg);
        let result = generate_thumbnail(&bytes, "image/jpeg", &default_config()).unwrap();

        assert!(result.width <= 128);
        assert!(result.height <= 128);
        // landscape: width should hit the limit, height should be ~96 (3/4)
        assert!(result.width >= result.height, "should remain landscape");
        assert!(!result.data.is_empty());
    }

    #[test]
    fn png_square_produces_webp() {
        let bytes = make_image_bytes(200, 200, ImageFormat::Png);
        let result = generate_thumbnail(&bytes, "image/png", &default_config()).unwrap();

        assert!(result.width <= 128);
        assert!(result.height <= 128);
        assert_eq!(result.width, result.height, "square should stay square");
    }

    #[test]
    fn webp_input_produces_webp() {
        let bytes = make_image_bytes(300, 100, ImageFormat::WebP);
        let result = generate_thumbnail(&bytes, "image/webp", &default_config()).unwrap();

        assert!(result.width <= 128);
        assert!(result.height <= 128);
        // 300:100 = 3:1, so width should be about 3x height
        assert!(result.width > result.height, "should remain landscape");
    }

    #[test]
    fn portrait_image_preserves_aspect_ratio() {
        let bytes = make_image_bytes(100, 400, ImageFormat::Png);
        let result = generate_thumbnail(&bytes, "image/png", &default_config()).unwrap();

        assert!(result.width <= 128);
        assert!(result.height <= 128);
        assert!(result.height > result.width, "should remain portrait");
    }

    #[test]
    fn small_image_fits_within_bounds() {
        let bytes = make_image_bytes(50, 50, ImageFormat::Png);
        let result = generate_thumbnail(&bytes, "image/png", &default_config()).unwrap();

        // image::thumbnail resizes to fit within config bounds
        assert!(result.width <= 128);
        assert!(result.height <= 128);
        assert_eq!(result.width, result.height, "square should stay square");
    }

    // ── Error paths ──

    #[test]
    fn too_many_pixels_rejected_before_decode() {
        // Use a small image but lower the threshold so pixel count exceeds it.
        let bytes = make_image_bytes(200, 200, ImageFormat::Png);
        let config = ThumbnailConfig {
            max_pixels: 10_000, // 200*200 = 40_000 > 10_000
            ..default_config()
        };
        let result = generate_thumbnail(&bytes, "image/png", &config);

        match result {
            Err(ThumbnailError::TooManyPixels { pixels, max }) => {
                assert_eq!(pixels, 40_000);
                assert_eq!(max, 10_000);
            }
            other => panic!("expected TooManyPixels, got {other:?}"),
        }
    }

    #[test]
    fn max_decode_bytes_limits_decoder() {
        // Create a valid image but set decoder limit very low
        let bytes = make_image_bytes(200, 200, ImageFormat::Png);
        let config = ThumbnailConfig {
            max_decode_bytes: 64, // way too small to decode 200x200
            ..default_config()
        };

        let result = generate_thumbnail(&bytes, "image/png", &config);
        assert!(
            matches!(result, Err(ThumbnailError::DecodeFailed(_))),
            "expected DecodeFailed, got {result:?}"
        );
    }

    #[test]
    fn corrupt_bytes_return_decode_failed() {
        let garbage = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01, 0x02, 0x03];
        let result = generate_thumbnail(&garbage, "image/png", &default_config());

        assert!(
            matches!(result, Err(ThumbnailError::DecodeFailed(_))),
            "expected DecodeFailed, got {result:?}"
        );
    }

    #[test]
    fn decoded_thumbnail_too_large_returns_error() {
        // 256×256 resized to fit 128×128 → 128×128 = 16384 pixels × 4 = 65536 decoded bytes.
        // Set max_bytes below that to trigger the guard.
        let bytes = make_image_bytes(256, 256, ImageFormat::Png);
        let config = ThumbnailConfig {
            max_bytes: 1024, // well below 65536 decoded bytes
            ..default_config()
        };

        let result = generate_thumbnail(&bytes, "image/png", &config);
        match result {
            Err(ThumbnailError::TooLarge { size, max }) => {
                // 128 * 128 * 4 = 65536
                assert_eq!(size, 65_536);
                assert_eq!(max, 1024);
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_format_gif_rejected() {
        let result = generate_thumbnail(&[0; 100], "image/gif", &default_config());

        match result {
            Err(ThumbnailError::UnsupportedFormat(fmt)) => {
                assert_eq!(fmt, "image/gif");
            }
            other => panic!("expected UnsupportedFormat, got {other:?}"),
        }
    }

    #[test]
    fn empty_bytes_return_decode_failed() {
        let result = generate_thumbnail(&[], "image/png", &default_config());

        assert!(
            matches!(result, Err(ThumbnailError::DecodeFailed(_))),
            "expected DecodeFailed, got {result:?}"
        );
    }
}
