//! Small helpers over `quick-xml` shared by the OOXML parsers.

use quick_xml::events::{BytesRef, BytesStart};
use quick_xml::{Reader, XmlVersion};

/// Decodes an XML part: UTF-16 when it starts with a UTF-16 BOM, otherwise
/// UTF-8 (BOM stripped). Valid UTF-8 is reused without a copy; invalid
/// sequences are replaced.
pub fn decode_part(mut bytes: Vec<u8>) -> String {
    match bytes.as_slice() {
        [0xFF, 0xFE, rest @ ..] => utf16_chars(rest, u16::from_le_bytes).collect(),
        [0xFE, 0xFF, rest @ ..] => utf16_chars(rest, u16::from_be_bytes).collect(),
        _ => {
            if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
                bytes.drain(..3);
            }
            String::from_utf8(bytes)
                .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
        }
    }
}

/// The length [`decode_part`] gives `bytes`, found without building it:
/// lossy decoding can triple the size.
pub fn decoded_len(bytes: &[u8]) -> usize {
    match bytes {
        [0xFF, 0xFE, rest @ ..] => utf16_chars(rest, u16::from_le_bytes)
            .map(char::len_utf8)
            .sum(),
        [0xFE, 0xFF, rest @ ..] => utf16_chars(rest, u16::from_be_bytes)
            .map(char::len_utf8)
            .sum(),
        _ => {
            let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
            // Lossy decoding puts one U+FFFD (3 bytes) for each invalid run.
            bytes
                .utf8_chunks()
                .map(|chunk| {
                    let replaced = if chunk.invalid().is_empty() { 0 } else { 3 };
                    chunk.valid().len() + replaced
                })
                .sum()
        }
    }
}

fn utf16_chars(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> impl Iterator<Item = char> + '_ {
    let (pairs, _odd_trailing_byte) = bytes.as_chunks::<2>();
    char::decode_utf16(pairs.iter().map(move |&pair| unit(pair)))
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
}

/// A reader over a decoded part. Text is not trimmed: whitespace inside runs
/// (`<w:t xml:space="preserve"> </w:t>`) is content.
pub fn reader(text: &str) -> Reader<&[u8]> {
    Reader::from_str(text)
}

/// The qualified element name as written (`w:p`, `Relationship`).
pub fn qname<'a>(e: &'a BytesStart<'_>) -> &'a str {
    e.name().into_inner()
}

/// The value of the attribute whose qualified name is exactly `name` (`w:val`,
/// `r:id`, `Id`). Predefined entities and character references are resolved; a
/// value with anything else is returned raw.
pub fn attr(e: &BytesStart<'_>, name: &str) -> Option<String> {
    find_attr(e, |key| key == name)
}

/// The element name without its prefix (`x:row` -> `row`). Only for parts
/// whose elements of interest all share one namespace: `SpreadsheetML` parts
/// are written both unprefixed and with an `x:` prefix.
pub fn local_name<'a>(e: &'a BytesStart<'_>) -> &'a str {
    let name = e.name().into_inner();
    name.rsplit_once(':').map_or(name, |(_, local)| local)
}

/// The value of a *prefixed* attribute with local name `local`, whatever the
/// prefix (`r:id`, `ns1:id`). Unprefixed attributes never match, so the
/// relationship id stays apart from a plain `id`.
pub fn prefixed_attr(e: &BytesStart<'_>, local: &str) -> Option<String> {
    find_attr(e, |key| {
        key.rsplit_once(':').is_some_and(|(_, l)| l == local)
    })
}

fn find_attr(e: &BytesStart<'_>, matches: impl Fn(&str) -> bool) -> Option<String> {
    e.attributes().with_checks(false).flatten().find_map(|a| {
        if !matches(a.key.into_inner()) {
            return None;
        }
        Some(match a.normalized_value(XmlVersion::Implicit1_0) {
            Ok(value) => value.into_owned(),
            Err(_) => a.value.into_owned(),
        })
    })
}

/// Text for an entity or character reference. Only the five predefined
/// entities and numeric references are resolved; a custom entity from a DTD
/// yields `None` and is dropped, so entity-expansion attacks have nothing to
/// expand.
pub fn resolve_ref(r: &BytesRef<'_>) -> Option<String> {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        return Some(c.to_string());
    }
    let content = r.xml10_content();
    quick_xml::escape::resolve_predefined_entity(&content).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use quick_xml::events::Event;

    /// Concatenated text of a document, the way the parsers assemble it.
    fn text_of(xml: &str) -> String {
        let mut reader = reader(xml);
        let mut out = String::new();
        loop {
            match reader.read_event().unwrap() {
                Event::Text(t) => out.push_str(&t.xml10_content()),
                Event::GeneralRef(r) => out.push_str(&resolve_ref(&r).unwrap_or_default()),
                Event::Eof => break,
                _ => {}
            }
        }
        out
    }

    #[test]
    fn decoded_len_matches_the_decoded_text() {
        let inputs: [&[u8]; 7] = [
            b"plain",
            b"\xEF\xBB\xBFbom",
            b"a\xFF\xFEb\xC3",
            b"\xE2\x82",
            b"\xFF\xFEa\x00\x3B\x04\x00\xD8x",
            b"\xFE\xFF\x00a\xD8\x3D\xDE\x00\x01",
            b"",
        ];
        for input in inputs {
            assert_eq!(
                decoded_len(input),
                decode_part(input.to_vec()).len(),
                "{input:?}"
            );
        }
    }

    #[test]
    fn predefined_and_char_refs_are_resolved() {
        assert_eq!(
            text_of("<t>a &amp; b &lt;c&gt; &#x41;&#66;</t>"),
            "a & b <c> AB"
        );
    }

    #[test]
    fn custom_entity_is_not_expanded() {
        let xml = r#"<?xml version="1.0"?>
<!DOCTYPE t [<!ENTITY a "AAAAAAAAAA"><!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;">
<!ENTITY xxe SYSTEM "file:///etc/passwd">]>
<t>x&b;&xxe;y</t>"#;
        // Whitespace between the prolog and the root element is text too.
        assert_eq!(text_of(xml).trim(), "xy");
    }

    #[test]
    fn utf16_and_bom_parts_are_decoded() {
        let mut le = vec![0xFF, 0xFE];
        le.extend("<a>\u{e9}</a>".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(decode_part(le), "<a>\u{e9}</a>");

        let mut be = vec![0xFE, 0xFF];
        be.extend("<a>\u{e9}</a>".encode_utf16().flat_map(u16::to_be_bytes));
        assert_eq!(decode_part(be), "<a>\u{e9}</a>");

        assert_eq!(decode_part(b"\xEF\xBB\xBF<a/>".to_vec()), "<a/>");
    }

    #[test]
    fn broken_utf16_and_utf8_are_replaced_not_rejected() {
        // Odd trailing byte is dropped; an unpaired surrogate becomes U+FFFD.
        let odd = vec![0xFF, 0xFE, b'a', 0x00, b'b'];
        assert_eq!(decode_part(odd), "a");
        let lone_surrogate = vec![0xFF, 0xFE, 0x00, 0xD8, b'x', 0x00];
        assert_eq!(decode_part(lone_surrogate), "\u{fffd}x");
        assert_eq!(decode_part(vec![b'a', 0xFF, b'b']), "a\u{fffd}b");
    }

    #[test]
    fn attr_matches_qualified_name_and_unescapes() {
        let xml = r#"<w:p xmlns:w="w" w:val="a&amp;b" r:id="rId7" plain="x"/>"#;
        let mut reader = reader(xml);
        let Event::Empty(e) = reader.read_event().unwrap() else {
            panic!("expected empty element");
        };
        assert_eq!(qname(&e), "w:p");
        assert_eq!(attr(&e, "w:val").as_deref(), Some("a&b"));
        assert_eq!(attr(&e, "r:id").as_deref(), Some("rId7"));
        assert_eq!(attr(&e, "plain").as_deref(), Some("x"));
        assert_eq!(attr(&e, "val"), None, "prefix is part of the name");
        assert_eq!(attr(&e, "missing"), None);
    }

    #[test]
    fn same_local_name_with_and_without_prefix_stays_apart() {
        // presentation.xml: the slide id and the relationship id share `id`.
        let mut reader = reader(r#"<p:sldId id="256" r:id="rId2"/>"#);
        let Event::Empty(e) = reader.read_event().unwrap() else {
            panic!("expected empty element");
        };
        assert_eq!(attr(&e, "r:id").as_deref(), Some("rId2"));
        assert_eq!(attr(&e, "id").as_deref(), Some("256"));
        assert_eq!(prefixed_attr(&e, "id").as_deref(), Some("rId2"));
        assert_eq!(local_name(&e), "sldId");
    }
}
