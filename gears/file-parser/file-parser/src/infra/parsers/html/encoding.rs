//! Picks the character encoding of an HTML document.
//!
//! Order, as in the HTML standard's encoding sniffing: a byte order mark, the
//! `charset` of the transport `Content-Type`, a `<meta>` declaration within the
//! first 1024 bytes; otherwise UTF-8 if the bytes are valid UTF-8, else
//! Windows-1252.

use encoding_rs::{Encoding, UTF_8, WINDOWS_1252};

/// How many leading bytes are searched for a `<meta>` charset.
const PRESCAN_BYTES: usize = 1024;

/// Decodes `bytes` to text.
pub(super) fn decode(bytes: &[u8], content_type: Option<&str>) -> String {
    if let Some((encoding, bom_len)) = Encoding::for_bom(bytes) {
        let (text, _) = encoding.decode_without_bom_handling(&bytes[bom_len..]);
        return text.into_owned();
    }
    let transport = content_type
        .and_then(charset_param)
        .and_then(|label| Encoding::for_label(label.as_bytes()));
    // A `<meta>` naming UTF-16 was read as ASCII, so it means UTF-8, and
    // `x-user-defined` means Windows-1252 (HTML standard, prescan).
    let declared = transport.or_else(|| {
        meta_charset(&bytes[..bytes.len().min(PRESCAN_BYTES)])
            .and_then(|label| Encoding::for_label(label.as_bytes()))
            .map(|e| {
                if e == encoding_rs::X_USER_DEFINED {
                    WINDOWS_1252
                } else {
                    e.output_encoding()
                }
            })
    });
    let encoding = declared.unwrap_or_else(|| {
        // A file cut off inside a UTF-8 sequence is still UTF-8.
        match std::str::from_utf8(bytes) {
            Ok(_) => UTF_8,
            Err(e) if e.error_len().is_none() => UTF_8,
            Err(_) => WINDOWS_1252,
        }
    });
    let (text, _) = encoding.decode_without_bom_handling(bytes);
    text.into_owned()
}

/// The `charset` parameter of a `Content-Type` value.
fn charset_param(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|param| {
        let (name, value) = param.split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches(['"', '\'']).to_owned())
            .filter(|v| !v.is_empty())
    })
}

/// The charset a `<meta charset>` or `<meta http-equiv="content-type">` tag
/// declares within `head`.
fn meta_charset(head: &[u8]) -> Option<String> {
    let lower = head.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = find(&lower[from..], b"<meta") {
        let tag_start = from + at;
        let tag_end = find(&lower[tag_start..], b">").map_or(lower.len(), |e| tag_start + e);
        let tag = &lower[tag_start..tag_end];
        // Every `charset` in the tag, until one is followed by a value.
        let mut search = 0;
        while let Some(at) = find(&tag[search..], b"charset") {
            let after = search + at + b"charset".len();
            search = after;
            let value = tag[after..].trim_ascii_start();
            let Some(value) = value.strip_prefix(b"=") else {
                continue;
            };
            let value = value.trim_ascii_start();
            let value = value
                .strip_prefix(b"\"")
                .or_else(|| value.strip_prefix(b"'"))
                .unwrap_or(value);
            let end = value
                .iter()
                .position(|&b| {
                    matches!(b, b'"' | b'\'' | b';' | b'/' | b'>') || b.is_ascii_whitespace()
                })
                .unwrap_or(value.len());
            if end > 0 {
                return String::from_utf8(value[..end].to_vec()).ok();
            }
        }
        from = tag_end.max(tag_start + 1);
    }
    None
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bom_wins() {
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend("<p>\u{e9}</p>".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(
            decode(&utf16, Some("text/html; charset=windows-1251")),
            "<p>\u{e9}</p>"
        );
        assert_eq!(decode(b"\xEF\xBB\xBFok", None), "ok");
    }

    #[test]
    fn encoding_from_content_type() {
        // 0xE4 is "д" in Windows-1251.
        assert_eq!(
            decode(b"<p>\xE4</p>", Some("text/html; charset=\"windows-1251\"")),
            "<p>\u{434}</p>"
        );
    }

    #[test]
    fn encoding_from_meta() {
        let doc = b"<html><head><meta charset=koi8-r></head><body>\xC4</body></html>";
        assert!(decode(doc, None).contains('\u{434}'));
        let doc =
            b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=windows-1251\">\xE4";
        assert!(decode(doc, None).ends_with('\u{434}'));
    }

    #[test]
    fn utf16_meta_label_means_utf8() {
        let doc = "<meta charset=utf-16><p>\u{e9}</p>".as_bytes();
        assert!(decode(doc, None).contains('\u{e9}'));
    }

    #[test]
    fn fallback_is_utf8_then_windows_1252() {
        assert_eq!(decode("\u{e9}".as_bytes(), None), "\u{e9}");
        // 0x80 is the euro sign in Windows-1252 and invalid UTF-8.
        assert_eq!(decode(b"\x80", None), "\u{20ac}");
    }

    #[test]
    fn encoding_nits() {
        // x-user-defined in a meta means Windows-1252.
        assert!(decode(b"<meta charset=x-user-defined>\x80", None).ends_with('\u{20ac}'));
        // The second `charset` of a tag carries the value.
        let doc = b"<meta name=charset content=\"text/html; charset=windows-1251\">\xE4";
        assert!(decode(doc, None).ends_with('\u{434}'));
        // A UTF-8 file cut inside a sequence stays UTF-8.
        let mut cut = "\u{e9}t\u{e9}".as_bytes().to_vec();
        cut.pop();
        assert!(decode(&cut, None).starts_with("\u{e9}t"));
    }

    #[test]
    fn meta_after_the_prescan_window_is_ignored() {
        let mut doc = vec![b' '; PRESCAN_BYTES];
        doc.extend_from_slice(b"<meta charset=windows-1251>\xE4!");
        // Not UTF-8, no declaration seen: Windows-1252.
        assert!(decode(&doc, None).ends_with("\u{e4}!"));
    }
}
