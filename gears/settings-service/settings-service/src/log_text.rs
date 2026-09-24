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
/// control character, the C1 range and `DEL` included, becomes `\u{..}`.
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
                c if c.is_control() => write!(f, "\\u{{{:x}}}", u32::from(c))?,
                c => f.write_char(c)?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "log_text_tests.rs"]
mod log_text_tests;
