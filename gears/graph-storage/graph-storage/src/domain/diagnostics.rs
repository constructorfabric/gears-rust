//! What a dependency's error may look like in a log line.
//!
//! Domain rather than infrastructure because every layer that logs a
//! dependency's text needs it -- the store and the engine, the domain
//! service's readiness and snapshot paths, the REST adapter's last-resort
//! handler -- and a domain module may not import `infra` (DE0301).

/// The longest rendering of a dependency's error that reaches a log line.
const LOGGED_MAX_CHARS: usize = 512;

/// The mark a cut rendering ends with.
const TRUNCATED: &str = " [truncated]";

/// A dependency's error, as it may be logged: control characters escaped,
/// and at most [`LOGGED_MAX_CHARS`] characters of the result plus the mark
/// that says it was cut.
///
/// A driver's or server's diagnostic is neither bounded nor free of control
/// characters, and the platform's default console format is text
/// (`ConsoleFormat::Text`), which writes a field's `Display` as it is -- so
/// an unbounded `%error` could split a line or fill a log. Only the JSON
/// console format and the file sink escape on their own, and a gear cannot
/// assume its deployment chose them.
///
/// The bound is on what is *emitted*, not on what is read: an escape is up
/// to six characters (`\u{0}`), so counting input characters let a text made
/// of control characters render at six times the bound.
pub(crate) fn logged(error: &dyn std::fmt::Display) -> String {
    let text = error.to_string();
    let mut out = String::with_capacity(text.len().min(LOGGED_MAX_CHARS) + TRUNCATED.len());
    let mut emitted = 0usize;
    for c in text.chars() {
        let width = if c.is_control() {
            c.escape_default().count()
        } else {
            1
        };
        if emitted + width > LOGGED_MAX_CHARS {
            out.push_str(TRUNCATED);
            break;
        }
        if c.is_control() {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
        emitted += width;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{LOGGED_MAX_CHARS, TRUNCATED, logged};

    /// A newline in a dependency's text does not start a new log record.
    #[test]
    fn control_characters_are_escaped() {
        let text = logged(&"relation \"kb\" does not exist\nERROR forged line");
        assert!(!text.contains('\n'), "{text}");
        assert!(text.contains("\\n"), "{text}");
    }

    /// A diagnostic longer than the bound is cut, and says so.
    #[test]
    fn long_text_is_bounded() {
        let text = logged(&"x".repeat(LOGGED_MAX_CHARS * 4));
        assert_eq!(
            text.chars().count(),
            LOGGED_MAX_CHARS + TRUNCATED.chars().count()
        );
        assert!(text.ends_with(TRUNCATED), "{text}");
    }

    /// Every control character is escaped, not only the newline: a tab or a
    /// carriage return reshapes a line, a NUL ends a C string, and the C1
    /// range is control too.
    #[test]
    fn every_control_character_is_escaped() {
        let text = logged(&"a\tb\rc\0d\u{85}e");
        assert!(
            text.chars().all(|c| !c.is_control()),
            "no control character reaches the log: {text:?}"
        );
        assert_eq!(text, "a\\tb\\rc\\u{0}d\\u{85}e");
    }

    /// The bound is on the rendering: a text of NULs escapes six to one and
    /// still stays inside it.
    #[test]
    fn escaping_does_not_grow_the_output_past_the_bound() {
        let text = logged(&"\0".repeat(LOGGED_MAX_CHARS));
        assert!(
            text.chars().count() <= LOGGED_MAX_CHARS + TRUNCATED.chars().count(),
            "{} chars",
            text.chars().count()
        );
        assert!(text.ends_with(TRUNCATED), "{text}");
        assert!(text.starts_with("\\u{0}\\u{0}"), "{text}");
    }

    /// Exactly the bound is not cut, and one past it is.
    #[test]
    fn the_bound_is_exact() {
        let at = logged(&"y".repeat(LOGGED_MAX_CHARS));
        assert_eq!(at.chars().count(), LOGGED_MAX_CHARS);
        assert!(!at.contains(TRUNCATED), "{at}");

        let past = logged(&"y".repeat(LOGGED_MAX_CHARS + 1));
        assert!(past.ends_with(TRUNCATED), "{past}");
        assert_eq!(
            past.chars().count(),
            LOGGED_MAX_CHARS + TRUNCATED.chars().count()
        );
    }

    /// Characters are counted, not bytes: a multibyte text is neither cut
    /// short nor split inside a character.
    #[test]
    fn multibyte_text_is_counted_in_characters() {
        let text = logged(&"\u{436}".repeat(LOGGED_MAX_CHARS));
        assert_eq!(text, "\u{436}".repeat(LOGGED_MAX_CHARS));
        let past = logged(&"\u{436}".repeat(LOGGED_MAX_CHARS + 1));
        assert!(past.ends_with(TRUNCATED), "{past}");
        assert_eq!(
            past.chars().count(),
            LOGGED_MAX_CHARS + TRUNCATED.chars().count()
        );
    }

    /// The input is any `Display`, rendered as it renders itself.
    #[test]
    fn any_display_is_accepted() {
        struct Diagnostic;
        impl std::fmt::Display for Diagnostic {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "code 42:\ndetail")
            }
        }
        assert_eq!(logged(&Diagnostic), "code 42:\\ndetail");
    }
}
