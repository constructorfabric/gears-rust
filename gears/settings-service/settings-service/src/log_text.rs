// Created: 2026-09-23 by Virtuozzo International GmbH
//! Text that may enter a log line.
//!
//! An error from a dependency carries whatever text the dependency chose — a
//! relayed HTTP body, a plugin's message, a driver's diagnostic — and a log
//! is a sequence of lines. Interpolated verbatim, a newline in that text ends
//! one line and starts another that the dependency wrote, and an escape
//! sequence redraws a terminal. [`LogSafe`] renders any `Display` on one line
//! with every control character escaped, so a log shows what was received and
//! nothing the receiver did not put there.

use std::fmt::{self, Display, Write as _};

/// A `Display` value rendered for a log line: control characters escaped,
/// everything else as it is.
///
/// `\n`, `\r` and `\t` become their two-character spellings; any other
/// control character, the C1 range and `DEL` included, becomes `\u{..}`, and
/// so does every Unicode bidirectional format character — not a control
/// character, but one a terminal may use to reorder what follows.
/// Letters of every script, punctuation and symbols pass through unchanged,
/// so a message stays readable.
pub struct LogSafe<'a, T: ?Sized>(pub &'a T);

impl<T: Display + ?Sized> Display for LogSafe<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in self.0.to_string().chars() {
            match c {
                '\n' => f.write_str("\\n")?,
                '\r' => f.write_str("\\r")?,
                '\t' => f.write_str("\\t")?,
                c if c.is_control() || is_bidi_format(c) => write!(f, "\\u{{{:x}}}", u32::from(c))?,
                c => f.write_char(c)?,
            }
        }
        Ok(())
    }
}

/// The Unicode bidirectional format characters: the Arabic letter mark, the
/// left-to-right and right-to-left marks, the embeddings and overrides, and
/// the isolates.
const fn is_bidi_format(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

#[cfg(test)]
#[path = "log_text_tests.rs"]
mod log_text_tests;
