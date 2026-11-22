//! Which Excel number formats display a number as a date, time or duration.
//!
//! An XLSX cell stores a date as a plain number; only its style's number format
//! says it is a date. Built-in formats are known by id; custom ones are
//! classified from their format code.

/// How a numeric cell is displayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Number,
    Date,
    DateTime,
    Time,
    /// Elapsed time (`[h]:mm:ss`): not wrapped at 24 hours.
    Duration,
}

/// Kind of a built-in number format id (ECMA-376 Part 1, 18.8.30, plus the
/// East Asian date ids Excel ships).
pub(super) fn builtin(id: u32) -> Kind {
    match id {
        14..=17 | 27..=31 | 34..=36 | 50..=58 => Kind::Date,
        18..=21 | 32 | 33 | 45 | 47 => Kind::Time,
        22 => Kind::DateTime,
        46 => Kind::Duration,
        _ => Kind::Number,
    }
}

/// Kind of a custom format code, judged from its first section.
pub(super) fn classify(code: &str) -> Kind {
    let section = first_section(code);
    if section.trim().eq_ignore_ascii_case("general") {
        return Kind::Number;
    }
    let scan = scan(section);
    let (date, time) = resolve(&scan.tokens, scan.twelve_hour);
    if scan.elapsed {
        Kind::Duration
    } else if date && time {
        Kind::DateTime
    } else if date {
        Kind::Date
    } else if time {
        Kind::Time
    } else {
        Kind::Number
    }
}

/// What a format section contains, literals and brackets aside.
struct Scan {
    /// `[h]`, `[mm]` or `[ss]`: elapsed time.
    elapsed: bool,
    /// `AM/PM` or `A/P`.
    twelve_hour: bool,
    /// Date and time letters in order, runs collapsed (`yyyy` -> `y`).
    tokens: Vec<char>,
}

fn scan(section: &str) -> Scan {
    let mut out = Scan {
        elapsed: false,
        twelve_hour: false,
        tokens: Vec::new(),
    };
    let mut chars = section.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            // Literal text, escaped character, padding and fill.
            '"' => {
                for (_, d) in chars.by_ref() {
                    if d == '"' {
                        break;
                    }
                }
            }
            '\\' | '_' | '*' => {
                chars.next();
            }
            // Colours, conditions and locale ids are not part of the pattern.
            '[' => {
                let mut inner = String::new();
                for (_, d) in chars.by_ref() {
                    if d == ']' {
                        break;
                    }
                    inner.push(d.to_ascii_lowercase());
                }
                if !inner.is_empty() && inner.chars().all(|d| matches!(d, 'h' | 'm' | 's')) {
                    out.elapsed = true;
                }
            }
            'y' | 'Y' | 'd' | 'D' | 'h' | 'H' | 's' | 'S' | 'm' | 'M' => {
                let c = c.to_ascii_lowercase();
                if out.tokens.last() != Some(&c) {
                    out.tokens.push(c);
                }
            }
            // The letters of `AM/PM` and `A/P` are not date tokens (the `M`
            // of `AM/PM` is no month).
            'a' | 'A' => {
                let rest = &section.as_bytes()[i..];
                let skip = if starts_with_ignore_case(rest, b"AM/PM") {
                    4
                } else if starts_with_ignore_case(rest, b"A/P") {
                    2
                } else {
                    0
                };
                if skip > 0 {
                    out.twelve_hour = true;
                    for _ in 0..skip {
                        chars.next();
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Whether the tokens show a date and a time. `m` is minutes right after
/// hours or right before seconds, otherwise months.
fn resolve(tokens: &[char], twelve_hour: bool) -> (bool, bool) {
    let mut date = false;
    let mut time = twelve_hour;
    for (i, &t) in tokens.iter().enumerate() {
        match t {
            'y' | 'd' => date = true,
            'h' | 's' => time = true,
            _ => {
                let after_hour = i > 0 && tokens[i - 1] == 'h';
                let before_second = tokens.get(i + 1) == Some(&'s');
                if after_hour || before_second {
                    time = true;
                } else {
                    date = true;
                }
            }
        }
    }
    (date, time)
}

fn starts_with_ignore_case(bytes: &[u8], prefix: &[u8]) -> bool {
    bytes
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

/// The part before the first unquoted `;` (the format for positive numbers).
fn first_section(code: &str) -> &str {
    let mut quoted = false;
    let mut escaped = false;
    for (i, c) in code.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '"' => quoted = !quoted,
            ';' if !quoted => return &code[..i],
            _ => {}
        }
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_format_codes_stay_linear() {
        // One copy of the rest of the code per letter would be quadratic and
        // not finish in test time.
        let code = "a".repeat(1_000_000);
        assert_eq!(classify(&code), Kind::Number);
    }

    #[test]
    fn builtin_ids() {
        assert_eq!(builtin(0), Kind::Number);
        assert_eq!(builtin(2), Kind::Number);
        assert_eq!(builtin(14), Kind::Date);
        assert_eq!(builtin(20), Kind::Time);
        assert_eq!(builtin(22), Kind::DateTime);
        assert_eq!(builtin(46), Kind::Duration);
        assert_eq!(builtin(49), Kind::Number, "49 is text (@)");
    }

    #[test]
    fn custom_codes() {
        assert_eq!(classify("yyyy-mm-dd"), Kind::Date);
        assert_eq!(classify("mmm"), Kind::Date);
        assert_eq!(classify("dd/mm/yyyy hh:mm"), Kind::DateTime);
        assert_eq!(classify("hh:mm:ss"), Kind::Time);
        assert_eq!(classify("mm:ss"), Kind::Time, "m before s is minutes");
        assert_eq!(classify("mm/dd hh:mm"), Kind::DateTime);
        assert_eq!(classify("h:mm AM/PM"), Kind::Time);
        assert_eq!(classify("h:mm:ss AM/PM"), Kind::Time);
        assert_eq!(classify("[$-F400]h:mm:ss\\ AM/PM"), Kind::Time);
        assert_eq!(classify("AM/PM h:mm"), Kind::Time);
        assert_eq!(classify("h:mm a/p"), Kind::Time);
        assert_eq!(classify("[h]:mm:ss"), Kind::Duration);
        assert_eq!(classify("[$-409]d-mmm-yy;@"), Kind::Date);
        assert_eq!(classify("[Red]0.00"), Kind::Number);
        assert_eq!(classify("0.00E+00"), Kind::Number);
        assert_eq!(
            classify("#,##0.00 \"days\""),
            Kind::Number,
            "quoted text is literal"
        );
        assert_eq!(classify("0\\d"), Kind::Number, "escaped letter is literal");
        assert_eq!(classify("General"), Kind::Number);
        assert_eq!(
            classify("0;\"yyyy\";0"),
            Kind::Number,
            "only the first section counts"
        );
    }
}
