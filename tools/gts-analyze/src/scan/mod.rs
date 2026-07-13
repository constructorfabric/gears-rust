pub mod json;
pub mod markdown;
pub mod rust;

use regex::Regex;
use std::sync::OnceLock;
use toolkit_gts::GTS_ID_PREFIX;

/// One GTS-segment grammar (matches the spec subset used elsewhere in the project).
/// Form: `<vendor>.<package>.<namespace>.<type>.v<MAJOR>[.<MINOR>]`.
const SEG: &str = concat!(
    r"[a-z_][a-z0-9_]*",
    r"\.[a-z_*][a-z0-9_*]*",
    r"\.[a-z_*][a-z0-9_*]*",
    r"\.[a-z_*][a-z0-9_*]*",
    r"\.v\d+(?:\.\d+)?",
);

fn id_with_wildcard() -> &'static str {
    static S: OnceLock<String> = OnceLock::new();
    S.get_or_init(|| format!(r"{SEG}(?:~{SEG})*~?(?:\.\*)?"))
}

/// Matches a GTS id inside a quoted string literal — used for `.json` files.
pub fn gts_in_string_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let body = id_with_wildcard();
        let prefix = regex::escape(GTS_ID_PREFIX);
        Regex::new(&format!(r#""({prefix}{body})""#)).expect("static regex")
    })
}

/// Matches a bare GTS id (no surrounding quotes) — used for `.md` files.
///
/// The `regex` crate does not support look-behind, so the leading word-boundary
/// is expressed as an alternation `(?:^|[^a-z0-9_])` of an outer non-capturing
/// group. Callers must use **group 1** for the matched id and its byte offset.
pub fn gts_bare_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let body = id_with_wildcard();
        let prefix = regex::escape(GTS_ID_PREFIX);
        Regex::new(&format!(r"(?:^|[^a-z0-9_])({prefix}{body})")).expect("static regex")
    })
}

/// True if a string passes the loose GTS-id heuristic used by the Rust scanner —
/// permits chained ids and `~`-suffixes, not a strict spec validator.
pub fn looks_like_gts_id(s: &str) -> bool {
    if s.len() > gts_id::GTS_ID_MAX_LENGTH {
        return false;
    }
    // Use the compile-time configured prefix instead of a
    // hard-coded literal so the heuristic tracks whatever prefix the GTS-id
    // crate was built with.
    if !s.starts_with(GTS_ID_PREFIX) {
        return false;
    }
    s.contains(".v")
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '~' | '*'))
}

/// Trim and ellipsise a markdown/JSON reference's context line —
/// caps at 120 chars and uses the 3-char ASCII ellipsis `...`,
/// matching the Python `scan_for_refs` function exactly.
pub fn shorten_line(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.chars().count() <= 120 {
        trimmed.to_string()
    } else {
        let mut out: String = trimmed.chars().take(117).collect();
        out.push_str("...");
        out
    }
}

/// Return 1-based line number for the given byte position, plus the line's content.
pub fn line_at(text: &str, byte_pos: usize) -> (usize, &str) {
    // Count newlines up to byte_pos (exclusive).
    let line_no = 1 + text[..byte_pos].bytes().filter(|b| *b == b'\n').count();
    let line_start = text[..byte_pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_end = text[byte_pos..]
        .find('\n')
        .map(|i| byte_pos + i)
        .unwrap_or(text.len());
    (line_no, &text[line_start..line_end])
}
