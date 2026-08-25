//! One reading of *"is this token in the crate's **code**, or only in its
//! prose"*, shared by the censuses that ask it.
//!
//! # Why this is a module and not a helper in the file that first needed it
//!
//! It was one: `approval_repo_tests` built it so its roster of
//! `AuditSubjectKind`s could tell a writer from a doc comment quoting one, and
//! its own doc names the property — *"prose that quotes a construction is not a
//! writer"*. The trigger registry's census then needed the same sentence and did
//! not have it, so `Trigger::BundleComposition` was **paid by two line comments
//! in `api::rest::bundles`**: deleting the crate's only construction of it, at
//! `infra::bundle`'s `composition_change_set`, would have left the census green
//! over prose alone. That is the state `bulkGroupMove` was in, which is the
//! defect that census exists to find.
//!
//! A second copy would have been two readings of "code" to disagree with each
//! other, on two censuses that exist to keep an attestation honest. So there is
//! one.
//!
//! # It is test-only, and the filename says so
//!
//! `_tests.rs` because both census walks skip that suffix when they enumerate the
//! crate's sources — the instrument must not be read as one of the sources it
//! measures. The module is named for what it does rather than for the file.

/// One file's text with every comment and every string, char and raw-string
/// literal replaced by spaces — **byte positions and newlines preserved**, so a
/// line number taken from the result is a line number in the input.
///
/// This is what makes a census syntactic rather than textual. Two concrete
/// properties depend on it: prose that quotes a construction is not a writer, and
/// a brace inside a literal cannot move an initializer's boundary.
///
/// A lifetime (`&'a str`) is deliberately not read as a char literal: a `'` opens
/// one only when the next character is an escape or is followed by a closing
/// quote, which is what distinguishes `'x'` from `'a` in the one place it matters.
pub fn blank_comments_and_literals(text: &str) -> String {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = String::with_capacity(text.len());
    let mut state = Lexed::Code;
    let mut index = 0;
    while index < chars.len() {
        let step = match state {
            Lexed::Code => step_code(&chars, index),
            Lexed::LineComment => step_line_comment(&chars, index),
            Lexed::BlockComment(depth) => step_block_comment(&chars, index, depth),
            Lexed::Str => step_quoted(&chars, index, '"'),
            Lexed::Char => step_quoted(&chars, index, '\''),
            Lexed::RawStr(hashes) => step_raw_str(&chars, index, hashes),
        };
        push_span(&mut out, &chars[index..index + step.consumed], step.keep);
        state = step.next;
        index += step.consumed;
    }
    out
}

/// The index of the `}` closing the `{` at `open`, counting depth.
///
/// Exact over [`blank_comments_and_literals`]'s output and **only** there: with
/// every comment and every literal already spaces, a brace is a brace. Over raw
/// text it is not a scanner at all — `"{"` in a string would open a block that
/// never closes.
///
/// It lives beside the blanking rather than in either caller for the reason the
/// module doc gives about the blanking itself: `approval_repo_tests` bounds a
/// `NewApproval` initializer with it so a `..template()` site cannot borrow the
/// next site's literal, and `triggers_tests` bounds a **function body** with it so
/// a producing site can be attributed to the function that contains it. Two copies
/// of a depth count would be two answers to *"where does this item end"* on two
/// censuses that exist to keep one attestation honest.
pub fn matching_brace(code: &str, open: usize) -> Option<usize> {
    matching_delim(code, open, b'{', b'}')
}

/// [`matching_brace`] over any delimiter pair — the same depth count, not a
/// second one.
///
/// The brace reading is the common case and keeps its own name; `authz_tests`
/// bounds an **argument list** with this one, to read the `owner_tenant_id` a
/// gate passes. Generalizing rather than copying is the module doc's own
/// argument: two depth counts would be two answers to *"where does this item
/// end"*.
pub fn matching_delim(code: &str, open: usize, open_ch: u8, close_ch: u8) -> Option<usize> {
    let mut depth = 0_usize;
    for (offset, byte) in code.as_bytes()[open..].iter().enumerate() {
        if *byte == open_ch {
            depth += 1;
        } else if *byte == close_ch {
            depth -= 1;
            if depth == 0 {
                return Some(open + offset);
            }
        }
    }
    None
}

/// What the scanner is inside of.
#[derive(Clone, Copy)]
enum Lexed {
    Code,
    LineComment,
    /// Rust block comments nest, so the depth is carried.
    BlockComment(usize),
    Str,
    Char,
    /// `r#"…"#`, carrying how many `#` close it.
    RawStr(usize),
}

/// One move: what the scanner is in next, how many chars it consumed, and whether
/// they are code (kept) or not (blanked).
struct Step {
    next: Lexed,
    consumed: usize,
    keep: bool,
}

/// Write a span, either as itself or as spaces — `\n` always as itself, which is
/// what preserves line numbers through a multi-line comment or string.
fn push_span(out: &mut String, span: &[(usize, char)], keep: bool) {
    for (_, c) in span {
        if keep {
            out.push(*c);
        } else if *c == '\n' {
            out.push('\n');
        } else {
            for _ in 0..c.len_utf8() {
                out.push(' ');
            }
        }
    }
}

fn char_at(chars: &[(usize, char)], index: usize) -> Option<char> {
    chars.get(index).map(|(_, c)| *c)
}

/// The only arm that keeps anything, and the only one that can open a literal.
fn step_code(chars: &[(usize, char)], index: usize) -> Step {
    let c = chars[index].1;
    let next = char_at(chars, index + 1);
    let blank = |state: Lexed, consumed: usize| Step {
        next: state,
        consumed,
        keep: false,
    };
    if c == '/' && next == Some('/') {
        return blank(Lexed::LineComment, 2);
    }
    if c == '/' && next == Some('*') {
        return blank(Lexed::BlockComment(1), 2);
    }
    if c == '"' {
        return blank(Lexed::Str, 1);
    }
    if let Some(step) = step_raw_str_prefix(chars, index) {
        return step;
    }
    // A `'` opens a char literal only when what follows is an escape or a single
    // character before the closing quote; otherwise it is a lifetime (`&'a str`),
    // and reading that as a literal would blank real code.
    if c == '\'' && (next == Some('\\') || char_at(chars, index + 2) == Some('\'')) {
        return blank(Lexed::Char, 1);
    }
    Step {
        next: Lexed::Code,
        consumed: 1,
        keep: true,
    }
}

/// `r"…"`, `r#"…"#`, `br"…"` and `br#"…"#`, if that is what starts here.
fn step_raw_str_prefix(chars: &[(usize, char)], index: usize) -> Option<Step> {
    let c = chars[index].1;
    if c != 'r' && c != 'b' {
        return None;
    }
    // Not a raw string if the `r` is the tail of an identifier (`for`, `iter`).
    if index > 0 && {
        let previous = chars[index - 1].1;
        previous.is_alphanumeric() || previous == '_'
    } {
        return None;
    }
    let mut look = index + 1;
    if c == 'b' && char_at(chars, look) == Some('r') {
        look += 1;
    } else if c == 'b' {
        // `b"…"` is an ordinary quote and `Lexed::Str` handles it from the `"`.
        return None;
    }
    let mut hashes = 0;
    while char_at(chars, look) == Some('#') {
        hashes += 1;
        look += 1;
    }
    if char_at(chars, look) != Some('"') {
        return None;
    }
    Some(Step {
        next: Lexed::RawStr(hashes),
        consumed: look - index + 1,
        keep: false,
    })
}

fn step_line_comment(chars: &[(usize, char)], index: usize) -> Step {
    Step {
        next: if chars[index].1 == '\n' {
            Lexed::Code
        } else {
            Lexed::LineComment
        },
        consumed: 1,
        keep: false,
    }
}

fn step_block_comment(chars: &[(usize, char)], index: usize, depth: usize) -> Step {
    let c = chars[index].1;
    let next = char_at(chars, index + 1);
    if c == '/' && next == Some('*') {
        return Step {
            next: Lexed::BlockComment(depth + 1),
            consumed: 2,
            keep: false,
        };
    }
    if c == '*' && next == Some('/') {
        return Step {
            next: if depth == 1 {
                Lexed::Code
            } else {
                Lexed::BlockComment(depth - 1)
            },
            consumed: 2,
            keep: false,
        };
    }
    Step {
        next: Lexed::BlockComment(depth),
        consumed: 1,
        keep: false,
    }
}

/// A `"…"` or `'…'` literal, whose escapes consume the escaped character too — so
/// `"\""` does not end at its middle quote.
fn step_quoted(chars: &[(usize, char)], index: usize, terminator: char) -> Step {
    let c = chars[index].1;
    let inside = if terminator == '"' {
        Lexed::Str
    } else {
        Lexed::Char
    };
    if c == '\\' && index + 1 < chars.len() {
        return Step {
            next: inside,
            consumed: 2,
            keep: false,
        };
    }
    Step {
        next: if c == terminator { Lexed::Code } else { inside },
        consumed: 1,
        keep: false,
    }
}

/// A raw string has no escapes; only `"` followed by its own `#` count ends it.
fn step_raw_str(chars: &[(usize, char)], index: usize, hashes: usize) -> Step {
    if chars[index].1 == '"' && (1..=hashes).all(|i| char_at(chars, index + i) == Some('#')) {
        return Step {
            next: Lexed::Code,
            consumed: hashes + 1,
            keep: false,
        };
    }
    Step {
        next: Lexed::RawStr(hashes),
        consumed: 1,
        keep: false,
    }
}

#[cfg(test)]
mod tests {
    use super::{blank_comments_and_literals, matching_brace, matching_delim};

    /// The invariant every census downstream rests on: a line number taken from
    /// the blanked text is a line number in the input.
    fn assert_shape_preserved(input: &str, blanked: &str) {
        assert_eq!(
            blanked.len(),
            input.len(),
            "byte length must survive, or every offset a census reports is wrong"
        );
        assert_eq!(
            blanked.matches('\n').count(),
            input.matches('\n').count(),
            "and so must the newlines, or every line number is wrong"
        );
    }

    #[test]
    fn every_lexed_arm_is_blanked_and_the_shape_survives() {
        // One case per arm of `Lexed`, in order: line comment, nested block
        // comment, string, char, raw string, byte raw string. `Code` is the arm
        // that must *not* be blanked, and the `let`s below are what say so.
        let input = concat!(
            "let a = 1; // one\n",
            "/* two /* nested */ still */\n",
            "let b = \"three\";\n",
            "let c = 'x';\n",
            "let d = r#\"four \"# ;\n",
            "let e = br\"five\";\n"
        );
        let blanked = blank_comments_and_literals(input);
        assert_shape_preserved(input, &blanked);

        for code in ["let a = 1;", "let b =", "let c =", "let d =", "let e ="] {
            assert!(
                blanked.contains(code),
                "code is kept: {code} missing from {blanked:?}"
            );
        }
        for prose in ["one", "two", "nested", "three", "four", "five"] {
            assert!(
                !blanked.contains(prose),
                "`{prose}` rode a comment or a literal and must be blanked: {blanked:?}"
            );
        }
    }

    /// A block comment nests, so the first `*/` does not close the outer one.
    ///
    /// Read as flat, `still */` would come back as code and a census would attribute
    /// whatever it quoted to a writer.
    #[test]
    fn a_nested_block_comment_closes_at_its_own_end() {
        let input = "a /* x /* y */ z */ b";
        let blanked = blank_comments_and_literals(input);
        assert_shape_preserved(input, &blanked);
        // The shape, not a hand-counted width: `a`, the whole comment as spaces,
        // then `b`. Read as flat, the trailing ` z */` would come back as code.
        assert!(blanked.starts_with("a "), "{blanked:?}");
        assert!(blanked.ends_with(" b"), "{blanked:?}");
        assert!(
            blanked[2..blanked.len() - 2].bytes().all(|b| b == b' '),
            "the whole nested comment is blanked: {blanked:?}"
        );
    }

    /// An escaped quote does not close its literal — the shape a census meets
    /// wherever the crate writes a quote into a message.
    #[test]
    fn an_escaped_terminator_does_not_close_the_literal() {
        for input in ["let s = \"a\\\"b\"; keep", "let c = '\\''; keep"] {
            let blanked = blank_comments_and_literals(input);
            assert_shape_preserved(input, &blanked);
            assert!(
                blanked.ends_with("; keep"),
                "the literal closed early and the tail was read as literal: {blanked:?}"
            );
            assert!(!blanked.contains('a'), "the body is blanked: {blanked:?}");
        }
    }

    /// **A lifetime is not a char literal**, which the module doc states and
    /// nothing measured.
    ///
    /// Read as one, `'a` opens a literal that runs to the next `'` in the file and
    /// blanks every construction between them - so a census would report a writer
    /// as absent because a lifetime appeared above it.
    #[test]
    fn a_lifetime_is_not_read_as_a_char_literal() {
        let input = "fn f<'a>(s: &'a str) -> &'a str { s } const K: char = 'z';";
        let blanked = blank_comments_and_literals(input);
        assert_shape_preserved(input, &blanked);
        assert!(
            blanked.contains("&'a str") && blanked.contains("{ s }"),
            "the lifetimes and the body between them are code: {blanked:?}"
        );
        assert!(
            !blanked.contains("'z'"),
            "while a real char literal is still blanked: {blanked:?}"
        );
    }

    /// A raw string closes on its own hash count, not on the first `"`.
    #[test]
    fn a_raw_string_closes_on_its_own_hash_count() {
        let input = "let s = r##\"a \"# b\"## ; keep";
        let blanked = blank_comments_and_literals(input);
        assert_shape_preserved(input, &blanked);
        assert!(
            blanked.ends_with("; keep"),
            "the inner `\"#` closed it early: {blanked:?}"
        );
        assert!(!blanked.contains('b'), "the body is blanked: {blanked:?}");
    }

    /// The second property the module doc names: **a brace inside a literal
    /// cannot move an item's boundary.**
    #[test]
    fn a_brace_inside_a_literal_does_not_move_the_boundary() {
        let raw = "fn f() { let s = \"}\"; } tail";
        let blanked = blank_comments_and_literals(raw);
        let open = blanked.find('{').expect("the body opens");

        let close = matching_brace(&blanked, open).expect("the body closes");
        assert_eq!(
            &raw[close..],
            "} tail",
            "the brace inside the string closed the body one item early"
        );

        // And the unblanked text is what it would have gone wrong on, which is why
        // the census blanks before it counts.
        assert_ne!(
            matching_brace(raw, open),
            Some(close),
            "the premise of blanking: over raw text the literal's brace wins"
        );
    }

    /// Depth, not the first closer — and `None` rather than a guess when the
    /// input never closes.
    #[test]
    fn matching_delim_counts_depth_and_answers_none_when_unbalanced() {
        let nested = "( a ( b ) c ) d";
        let close = matching_delim(nested, 0, b'(', b')').expect("it closes");
        assert_eq!(&nested[close..], ") d", "the inner `)` is not the match");

        assert_eq!(
            matching_delim("( a ( b ) c", 0, b'(', b')'),
            None,
            "an item that never closes has no end, and a guess would bound the \
             next one's text into this one"
        );
        assert_eq!(matching_brace("{ a", 0), None);
    }
}
