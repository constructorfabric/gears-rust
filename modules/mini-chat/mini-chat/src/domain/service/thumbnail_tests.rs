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
