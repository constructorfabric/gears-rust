//! A forgiving HTML tokenizer, linear in its input.
//!
//! It follows the HTML standard where that matters for extracting text
//! (comments, including `<!-->`; raw-text and escapable raw-text elements;
//! quoted and unquoted attribute values; named and numeric character
//! references) and keeps only the attributes the converter reads, so a tag
//! with a hundred thousand attributes costs one pass. Every step consumes at
//! least one byte.

/// Attributes the converter uses; all others are skipped while tokenizing.
const KEPT_ATTRIBUTES: [&str; 11] = [
    "href",
    "src",
    "alt",
    "title",
    "class",
    "hidden",
    "aria-hidden",
    "style",
    "colspan",
    "rowspan",
    "lang",
];

/// Longest attribute value kept; longer values (inline data) are cut.
const MAX_ATTRIBUTE_LEN: usize = 4096;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Token {
    Text(String),
    Start {
        name: String,
        attrs: Vec<(&'static str, String)>,
    },
    End {
        name: String,
    },
}

/// How the content after a start tag is read.
fn content_kind(name: &str) -> Content {
    match name {
        "script" | "style" | "xmp" | "iframe" | "noembed" | "noframes" | "noscript" => Content::Raw,
        "title" | "textarea" => Content::EscapableRaw,
        "plaintext" => Content::Plaintext,
        _ => Content::Markup,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Content {
    Markup,
    /// Text up to the matching end tag, references left as written.
    Raw,
    /// Text up to the matching end tag, references decoded.
    EscapableRaw,
    /// Everything to the end of the input is text.
    Plaintext,
}

pub(super) struct Tokenizer<'a> {
    input: &'a str,
    pos: usize,
    /// Set after a raw-text start tag: (element name, kind).
    pending: Option<(String, Content)>,
}

impl<'a> Tokenizer<'a> {
    pub(super) fn new(input: &'a str) -> Self {
        Self {
            input,
            pos: 0,
            pending: None,
        }
    }

    fn bytes(&self) -> &'a [u8] {
        self.input.as_bytes()
    }

    /// Text of a raw-text element: up to `</name` followed by whitespace, `/`
    /// or `>` (ASCII case-insensitive), or the end of the input.
    fn raw_text(&mut self, name: &str, kind: Content) -> Token {
        let bytes = self.bytes();
        let start = self.pos;
        let mut end = bytes.len();
        if kind != Content::Plaintext {
            let mut i = start;
            while let Some(at) = find_from(bytes, i, b"</") {
                let name_end = at + 2 + name.len();
                let matches = bytes
                    .get(at + 2..name_end)
                    .is_some_and(|n| n.eq_ignore_ascii_case(name.as_bytes()))
                    && bytes
                        .get(name_end)
                        .is_none_or(|b| b.is_ascii_whitespace() || *b == b'/' || *b == b'>');
                if matches {
                    end = at;
                    break;
                }
                i = at + 2;
            }
        }
        self.pos = end;
        let text = &self.input[start..end];
        Token::Text(if kind == Content::EscapableRaw {
            decode_references(text, false)
        } else {
            text.to_owned()
        })
    }

    /// Skips a comment starting at `<!--`; `<!-->` and `<!--->` are complete.
    fn comment(&mut self) {
        let bytes = self.bytes();
        let body = self.pos + 4;
        if bytes.get(body) == Some(&b'>') {
            self.pos = body + 1;
            return;
        }
        if bytes.get(body..body + 2) == Some(b"->") {
            self.pos = body + 2;
            return;
        }
        // One pass over the `--` occurrences: `-->` or `--!>` ends it.
        let mut i = body;
        while let Some(at) = find_from(bytes, i, b"--") {
            match bytes.get(at + 2) {
                Some(b'>') => {
                    self.pos = at + 3;
                    return;
                }
                Some(b'!') if bytes.get(at + 3) == Some(&b'>') => {
                    self.pos = at + 4;
                    return;
                }
                _ => i = at + 1,
            }
        }
        self.pos = bytes.len();
    }

    /// Reads a tag whose name starts at `name_start`.
    fn tag(&mut self, name_start: usize, end_tag: bool) -> Token {
        let bytes = self.bytes();
        let mut i = name_start;
        while i < bytes.len()
            && !matches!(
                bytes[i],
                b'\t' | b'\n' | b'\x0C' | b'\r' | b' ' | b'/' | b'>'
            )
        {
            i += 1;
        }
        let name = self.input[name_start..i].to_ascii_lowercase();
        let mut attrs: Vec<(&'static str, String)> = Vec::new();
        // Attributes, up to the closing `>`.
        loop {
            while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
                i += 1;
            }
            if i >= bytes.len() {
                break;
            }
            if bytes[i] == b'>' {
                i += 1;
                break;
            }
            // Attribute name: a leading `=` belongs to it, as in the standard.
            let attr_start = i;
            i += 1;
            while i < bytes.len()
                && !matches!(
                    bytes[i],
                    b'\t' | b'\n' | b'\x0C' | b'\r' | b' ' | b'/' | b'>' | b'='
                )
            {
                i += 1;
            }
            let attr_name = &self.input[attr_start..i];
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let mut value: Option<&str> = None;
            if bytes.get(i) == Some(&b'=') {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                match bytes.get(i) {
                    Some(&q @ (b'"' | b'\'')) => {
                        let start = i + 1;
                        let end = bytes[start..]
                            .iter()
                            .position(|&b| b == q)
                            .map_or(bytes.len(), |at| start + at);
                        value = Some(&self.input[start..end]);
                        i = (end + 1).min(bytes.len());
                    }
                    Some(_) => {
                        let start = i;
                        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>'
                        {
                            i += 1;
                        }
                        value = Some(&self.input[start..i]);
                    }
                    None => {}
                }
            }
            if !end_tag
                && let Some(kept) = KEPT_ATTRIBUTES
                    .iter()
                    .find(|k| k.eq_ignore_ascii_case(attr_name))
                // The first occurrence wins, as in the standard.
                && !attrs.iter().any(|(k, _)| k == kept)
            {
                let raw = value.unwrap_or("");
                let raw = &raw[..floor_char_boundary(raw, MAX_ATTRIBUTE_LEN)];
                attrs.push((kept, decode_references(raw, true)));
            }
        }
        self.pos = i;
        if end_tag {
            Token::End { name }
        } else {
            let kind = content_kind(&name);
            if kind != Content::Markup {
                self.pending = Some((name.clone(), kind));
            }
            Token::Start { name, attrs }
        }
    }
}

impl Iterator for Tokenizer<'_> {
    type Item = Token;

    fn next(&mut self) -> Option<Token> {
        if let Some((name, kind)) = self.pending.take()
            && self.pos < self.bytes().len()
        {
            return Some(self.raw_text(&name, kind));
        }
        let bytes = self.bytes();
        loop {
            if self.pos >= bytes.len() {
                return None;
            }
            let start = self.pos;
            if bytes[start] != b'<' {
                let end = bytes[start..]
                    .iter()
                    .position(|&b| b == b'<')
                    .map_or(bytes.len(), |at| start + at);
                self.pos = end;
                return Some(Token::Text(decode_references(
                    &self.input[start..end],
                    false,
                )));
            }
            match bytes.get(start + 1) {
                Some(b'!') if bytes.get(start + 2..start + 4) == Some(b"--") => self.comment(),
                // Doctype, CDATA outside foreign content, processing
                // instruction: a bogus comment up to `>`.
                Some(b'!' | b'?') => {
                    self.pos = find_from(bytes, start, b">").map_or(bytes.len(), |at| at + 1);
                }
                Some(b'/') => match bytes.get(start + 2) {
                    Some(b) if b.is_ascii_alphabetic() => return Some(self.tag(start + 2, true)),
                    Some(b'>') => self.pos = start + 3,
                    _ => {
                        self.pos = find_from(bytes, start, b">").map_or(bytes.len(), |at| at + 1);
                    }
                },
                Some(b) if b.is_ascii_alphabetic() => return Some(self.tag(start + 1, false)),
                // A `<` that starts no tag is text.
                _ => {
                    self.pos = start + 1;
                    return Some(Token::Text("<".to_owned()));
                }
            }
        }
    }
}

fn find_from(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    let rest = haystack.get(from..)?;
    rest.windows(needle.len())
        .position(|w| w == needle)
        .map(|at| from + at)
}

fn floor_char_boundary(s: &str, max: usize) -> usize {
    if s.len() <= max {
        return s.len();
    }
    let mut at = max;
    while !s.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The names the HTML standard still recognises without the closing `;`
/// (the HTML 4 Latin-1 set and the markup characters).
const LEGACY_NAMES: [&str; 106] = [
    "amp", "lt", "gt", "quot", "AMP", "LT", "GT", "QUOT", "COPY", "REG", "nbsp", "iexcl", "cent",
    "pound", "curren", "yen", "brvbar", "sect", "uml", "copy", "ordf", "laquo", "not", "shy",
    "reg", "macr", "deg", "plusmn", "sup2", "sup3", "acute", "micro", "para", "middot", "cedil",
    "sup1", "ordm", "raquo", "frac14", "frac12", "frac34", "iquest", "Agrave", "Aacute", "Acirc",
    "Atilde", "Auml", "Aring", "AElig", "Ccedil", "Egrave", "Eacute", "Ecirc", "Euml", "Igrave",
    "Iacute", "Icirc", "Iuml", "ETH", "Ntilde", "Ograve", "Oacute", "Ocirc", "Otilde", "Ouml",
    "times", "Oslash", "Ugrave", "Uacute", "Ucirc", "Uuml", "Yacute", "THORN", "szlig", "agrave",
    "aacute", "acirc", "atilde", "auml", "aring", "aelig", "ccedil", "egrave", "eacute", "ecirc",
    "euml", "igrave", "iacute", "icirc", "iuml", "eth", "ntilde", "ograve", "oacute", "ocirc",
    "otilde", "ouml", "divide", "oslash", "ugrave", "uacute", "ucirc", "uuml", "yacute", "thorn",
    "yuml",
];

/// Decodes character references (`&amp;`, `&eacute;`, `&#233;`, `&#xE9;`).
/// Anything that is not a recognised reference stays as written. In an
/// attribute value a legacy name without `;` followed by a letter, digit or
/// `=` is left alone, so query strings such as `?a=1&copy=2` survive.
pub(super) fn decode_references(text: &str, in_attribute: bool) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        if let Some((decoded, len)) = reference(rest, in_attribute) {
            out.push_str(&decoded);
            rest = &rest[len..];
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

/// The text of the reference at the start of `s` (which begins with `&`) and
/// its length in bytes.
fn reference(s: &str, in_attribute: bool) -> Option<(String, usize)> {
    let body = &s[1..];
    if let Some(num) = body.strip_prefix('#') {
        let (digits, radix, prefix) = match num.strip_prefix(['x', 'X']) {
            Some(hex) => (hex, 16, 2),
            None => (num, 10, 1),
        };
        let len = digits
            .bytes()
            .take_while(|b| {
                if radix == 16 {
                    b.is_ascii_hexdigit()
                } else {
                    b.is_ascii_digit()
                }
            })
            .count();
        if len == 0 {
            return None;
        }
        // Every digit belongs to the reference; a value too large for a
        // code point becomes U+FFFD.
        let value = u32::from_str_radix(&digits[..len], radix).unwrap_or(u32::MAX);
        let semicolon = usize::from(digits.as_bytes().get(len) == Some(&b';'));
        return Some((numeric(value).to_string(), 1 + prefix + len + semicolon));
    }
    let name_len = body
        .bytes()
        .take(32)
        .take_while(u8::is_ascii_alphanumeric)
        .count();
    if name_len == 0 {
        return None;
    }
    let name = &body[..name_len];
    if body.as_bytes().get(name_len) == Some(&b';')
        && let Some(decoded) = quick_xml::escape::resolve_html5_entity(name)
    {
        return Some((decoded.to_owned(), 1 + name_len + 1));
    }
    // The longest legacy name the text starts with (`&ampx;` is `&` + `x;`).
    let legacy = LEGACY_NAMES
        .iter()
        .filter(|legacy| name.starts_with(**legacy))
        .max_by_key(|legacy| legacy.len())?;
    let next = body.as_bytes().get(legacy.len());
    if in_attribute && next.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'=') {
        return None;
    }
    let decoded = quick_xml::escape::resolve_html5_entity(legacy)?;
    Some((decoded.to_owned(), 1 + legacy.len()))
}

/// The character a numeric reference stands for, with the standard's fixes:
/// NUL, surrogates and out-of-range values become U+FFFD, and 0x80-0x9F are
/// read as Windows-1252.
fn numeric(value: u32) -> char {
    if (0x80..=0x9F).contains(&value) {
        let byte = [u8::try_from(value).unwrap_or(0)];
        let (decoded, _) = encoding_rs::WINDOWS_1252.decode_without_bom_handling(&byte);
        return decoded
            .chars()
            .next()
            .unwrap_or(char::REPLACEMENT_CHARACTER);
    }
    match value {
        0 => char::REPLACEMENT_CHARACTER,
        v => char::from_u32(v).unwrap_or(char::REPLACEMENT_CHARACTER),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(html: &str) -> Vec<Token> {
        Tokenizer::new(html).collect()
    }

    fn start(name: &str, attrs: &[(&'static str, &str)]) -> Token {
        Token::Start {
            name: name.to_owned(),
            attrs: attrs.iter().map(|(k, v)| (*k, (*v).to_owned())).collect(),
        }
    }

    fn text(t: &str) -> Token {
        Token::Text(t.to_owned())
    }

    fn end(name: &str) -> Token {
        Token::End {
            name: name.to_owned(),
        }
    }

    #[test]
    fn tags_text_and_attributes() {
        assert_eq!(
            tokens(r#"<A HREF="x?a=1&amp;b=2" data-x=y class=c>t</a>"#),
            [
                start("a", &[("href", "x?a=1&b=2"), ("class", "c")]),
                text("t"),
                end("a")
            ]
        );
    }

    #[test]
    fn first_duplicate_attribute_wins_and_unknown_ones_are_dropped() {
        let many = (0..100_000).fold(String::new(), |mut acc, i| {
            use std::fmt::Write as _;
            write!(acc, " a{i}=v").unwrap();
            acc
        });
        let html = format!(r#"<p title="one" title="two"{many}>x"#);
        assert_eq!(tokens(&html), [start("p", &[("title", "one")]), text("x")]);
    }

    #[test]
    fn comments_including_abrupt_ones() {
        assert_eq!(tokens("a<!-- <p> -->b"), [text("a"), text("b")]);
        assert_eq!(tokens("<!--><p>"), [start("p", &[])]);
        assert_eq!(tokens("<!---><p>"), [start("p", &[])]);
        assert_eq!(tokens("<!-- x --!><p>"), [start("p", &[])]);
        assert!(tokens("<!-- never closed <p>").is_empty());
    }

    #[test]
    fn many_comments_stay_linear() {
        // Searching for each comment's end from scratch would be quadratic.
        let html = "<!--a-->".repeat(200_000);
        assert!(tokens(&html).is_empty());
    }

    #[test]
    fn quotes_outside_values_do_not_start_strings() {
        assert_eq!(tokens("<div \"x><p>"), [start("div", &[]), start("p", &[])]);
    }

    #[test]
    fn raw_text_elements() {
        assert_eq!(
            tokens("<script>if (a<b) { x = '</div>'; }</SCRIPT >after"),
            [
                start("script", &[]),
                text("if (a<b) { x = '</div>'; }"),
                end("script"),
                text("after")
            ]
        );
        assert_eq!(
            tokens("<title>a &amp; <b></title>"),
            [start("title", &[]), text("a & <b>"), end("title")]
        );
        assert_eq!(
            tokens("<style>p{}</stylex></style>"),
            [start("style", &[]), text("p{}</stylex>"), end("style")]
        );
        assert_eq!(
            tokens("<plaintext><p>rest"),
            [start("plaintext", &[]), text("<p>rest")]
        );
    }

    #[test]
    fn stray_lt_and_odd_markup() {
        assert_eq!(
            tokens("a < b <3 </> <!doctype html><?pi?></ >c"),
            [
                text("a "),
                text("<"),
                text(" b "),
                text("<"),
                text("3 "),
                text(" "),
                text("c"),
            ]
        );
    }

    #[test]
    fn character_references() {
        assert_eq!(
            decode_references(
                "&lt;&eacute;&#233;&#xE9;&#x80;&#0;&#x110000;&amp x &unknown; &",
                false
            ),
            "<\u{e9}\u{e9}\u{e9}\u{20ac}\u{fffd}\u{fffd}& x &unknown; &"
        );
        assert_eq!(decode_references("&ampersand", false), "&ersand");
        assert_eq!(
            decode_references("&auml &notin; &notit; &ampx;", false),
            "\u{e4} \u{2209} \u{ac}it; &x;"
        );
        // In attribute values, query-string parameters are not references.
        assert_eq!(
            decode_references("/x?id=1&region=us&copy=2&amp;lt", true),
            "/x?id=1&region=us&copy=2&lt"
        );
        assert_eq!(decode_references("a&copy b", true), "a\u{a9} b");
        assert_eq!(decode_references("&#99999999999", false), "\u{fffd}");
    }

    #[test]
    fn long_attribute_values_are_cut() {
        let long = format!("<img src=\"{}\">", "a".repeat(10_000));
        let Some(Token::Start { attrs, .. }) = tokens(&long).into_iter().next() else {
            panic!("expected a start tag");
        };
        assert_eq!(attrs[0].1.len(), MAX_ATTRIBUTE_LEN);
    }
}
