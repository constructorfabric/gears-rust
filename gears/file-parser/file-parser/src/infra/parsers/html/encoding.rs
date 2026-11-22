//! Picks the character encoding of an HTML document.
//!
//! Order, as in the HTML standard's encoding sniffing: a byte order mark, the
//! `charset` of the transport `Content-Type`, a `<meta>` declaration within the
//! first 1024 bytes; otherwise UTF-8 if the bytes are valid UTF-8, else
//! Windows-1252.

use encoding_rs::{Encoding, UTF_8, WINDOWS_1252};

/// How many leading bytes are searched for a `<meta>` charset.
const PRESCAN_BYTES: usize = 1024;

/// Decodes `bytes` to text, with CRLF and lone CR line breaks turned into
/// LF, as the HTML standard preprocesses its input.
pub(super) fn decode(bytes: &[u8], content_type: Option<&str>) -> String {
    let text = decode_text(bytes, content_type);
    if !text.contains('\r') {
        return text;
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            chars.next_if_eq(&'\n');
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}

fn decode_text(bytes: &[u8], content_type: Option<&str>) -> String {
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
        meta_charset(&bytes[..bytes.len().min(PRESCAN_BYTES)]).map(|e| {
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

/// The encoding a `<meta charset>` or `<meta http-equiv="content-type"
/// content="...; charset=...">` tag declares within `head`, found as the HTML
/// standard's prescan does: attributes are parsed, so a `charset=` in other
/// attribute values or in comments is not a declaration.
fn meta_charset(head: &[u8]) -> Option<&'static Encoding> {
    let bytes = head.to_ascii_lowercase();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &bytes[i..];
        if rest.starts_with(b"<!--") {
            // `<!-->` is a whole comment: the `-->` may reuse its dashes.
            i = find(&bytes[i + 2..], b"-->").map_or(bytes.len(), |at| i + 2 + at + 3);
        } else if rest.starts_with(b"<meta")
            && rest
                .get(5)
                .is_some_and(|&b| b.is_ascii_whitespace() || b == b'/')
        {
            i += 5;
            if let Some(encoding) = meta_attributes(&bytes, &mut i) {
                return Some(encoding);
            }
        } else if rest[0] == b'<'
            && (rest.get(1).is_some_and(u8::is_ascii_alphabetic)
                || (rest.get(1) == Some(&b'/') && rest.get(2).is_some_and(u8::is_ascii_alphabetic)))
        {
            // Any other tag: its attributes are skipped, quoted values with
            // a `>` included.
            i += 1;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                i += 1;
            }
            while attribute(&bytes, &mut i).is_some() {}
            i += 1;
        } else if rest.starts_with(b"<!") || rest.starts_with(b"</") || rest.starts_with(b"<?") {
            i = find(&bytes[i..], b">").map_or(bytes.len(), |at| i + at + 1);
        } else {
            i += 1;
        }
    }
    None
}

/// The encoding a `<meta>` tag declares, its attributes read from `i`; `i`
/// ends past the tag.
fn meta_attributes(bytes: &[u8], i: &mut usize) -> Option<&'static Encoding> {
    let mut seen: [bool; 3] = [false; 3];
    let mut got_pragma = false;
    // Whether the charset came from `content`, which needs the pragma.
    let mut need_pragma = None;
    let mut charset: Option<&[u8]> = None;
    while let Some((name, value)) = attribute(bytes, i) {
        let index = match name {
            b"http-equiv" => 0,
            b"content" => 1,
            b"charset" => 2,
            _ => continue,
        };
        // The first of a repeated attribute counts.
        if std::mem::replace(&mut seen[index], true) {
            continue;
        }
        match index {
            0 => got_pragma = value == b"content-type",
            1 => {
                if charset.is_none()
                    && let Some(found) = content_charset(value)
                {
                    charset = Some(found);
                    need_pragma = Some(true);
                }
            }
            _ => {
                charset = Some(value);
                need_pragma = Some(false);
            }
        }
    }
    *i += 1;
    match need_pragma {
        Some(true) if !got_pragma => None,
        Some(_) => charset.and_then(Encoding::for_label),
        None => None,
    }
}

/// The next attribute of a tag at `i` as (name, value), or `None` at the
/// tag's `>` or the end of the input. Every call that returns an attribute
/// moves `i` forward.
fn attribute<'a>(bytes: &'a [u8], i: &mut usize) -> Option<(&'a [u8], &'a [u8])> {
    let at = |i: usize| bytes.get(i).copied();
    while at(*i).is_some_and(|b| b.is_ascii_whitespace() || b == b'/') {
        *i += 1;
    }
    if at(*i).is_none_or(|b| b == b'>') {
        return None;
    }
    // A leading `=` belongs to the name, as in the standard.
    let start = *i;
    *i += 1;
    while at(*i).is_some_and(|b| !(b.is_ascii_whitespace() || matches!(b, b'/' | b'>' | b'='))) {
        *i += 1;
    }
    let name = &bytes[start..*i];
    while at(*i).is_some_and(|b| b.is_ascii_whitespace()) {
        *i += 1;
    }
    if at(*i) != Some(b'=') {
        return Some((name, b""));
    }
    *i += 1;
    while at(*i).is_some_and(|b| b.is_ascii_whitespace()) {
        *i += 1;
    }
    let value = match at(*i) {
        Some(q @ (b'"' | b'\'')) => {
            let from = *i + 1;
            let end = bytes[from..]
                .iter()
                .position(|&b| b == q)
                .map_or(bytes.len(), |e| from + e);
            *i = (end + 1).min(bytes.len());
            &bytes[from..end]
        }
        Some(b'>') | None => b"",
        Some(_) => {
            let from = *i;
            while at(*i).is_some_and(|b| !b.is_ascii_whitespace() && b != b'>') {
                *i += 1;
            }
            &bytes[from..*i]
        }
    };
    Some((name, value))
}

/// The `charset=` value in a `content` attribute.
fn content_charset(content: &[u8]) -> Option<&[u8]> {
    let mut from = 0;
    loop {
        let after = from + find(&content[from..], b"charset")? + b"charset".len();
        from = after;
        let Some(value) = content[after..].trim_ascii_start().strip_prefix(b"=") else {
            continue;
        };
        let value = value.trim_ascii_start();
        if let Some(&q @ (b'"' | b'\'')) = value.first() {
            let value = &value[1..];
            return value.iter().position(|&b| b == q).map(|end| &value[..end]);
        }
        let end = value
            .iter()
            .position(|&b| b.is_ascii_whitespace() || b == b';')
            .unwrap_or(value.len());
        return Some(&value[..end]).filter(|v| !v.is_empty());
    }
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
        // The second `charset` of a `content` value carries the value.
        let doc = b"<meta http-equiv=content-type content=\"charset; charset=windows-1251\">\xE4";
        assert!(decode(doc, None).ends_with('\u{434}'));
        // A UTF-8 file cut inside a sequence stays UTF-8.
        let mut cut = "\u{e9}t\u{e9}".as_bytes().to_vec();
        cut.pop();
        assert!(decode(&cut, None).starts_with("\u{e9}t"));
    }

    #[test]
    fn only_meta_declarations_name_the_charset() {
        // 0xE4 is "д" in Windows-1251 and "ä" in Windows-1252.
        let utf8 = "<meta name=\"description\" content=\"How to set charset=iso-8859-1\">\u{e9}";
        assert!(decode(utf8.as_bytes(), None).ends_with('\u{e9}'));
        // `content` counts only with `http-equiv=content-type`.
        let doc = b"<meta name=charset content=\"text/html; charset=windows-1251\">\xE4!";
        assert!(decode(doc, None).ends_with("\u{e4}!"));
        // A meta in a comment, or `charset=` in another tag, declares nothing.
        let doc = b"<!-- <meta charset=windows-1251> --><p title='charset=windows-1251'>\xE4!";
        assert!(decode(doc, None).ends_with("\u{e4}!"));
        // A `>` in a quoted value does not end the tag early.
        let doc = b"<p title='a>b <meta charset=windows-1251>'><meta charset=koi8-r>\xC4";
        assert!(decode(doc, None).ends_with('\u{434}'));
        // An unknown label is passed over for the next declaration.
        let doc = b"<meta charset=nonsense><meta charset=windows-1251>\xE4";
        assert!(decode(doc, None).ends_with('\u{434}'));
        // The `charset` attribute wins over `content`, whatever the order.
        let doc = b"<meta content=\"text/html; charset=koi8-r\" http-equiv=Content-Type charset=windows-1251>\xE4";
        assert!(decode(doc, None).ends_with('\u{434}'));
    }

    #[test]
    fn line_breaks_become_line_feeds() {
        assert_eq!(decode(b"a\r\nb\rc\r\r\nd\n", None), "a\nb\nc\n\nd\n");
    }

    #[test]
    fn meta_after_the_prescan_window_is_ignored() {
        let mut doc = vec![b' '; PRESCAN_BYTES];
        doc.extend_from_slice(b"<meta charset=windows-1251>\xE4!");
        // Not UTF-8, no declaration seen: Windows-1252.
        assert!(decode(&doc, None).ends_with("\u{e4}!"));
    }
}
