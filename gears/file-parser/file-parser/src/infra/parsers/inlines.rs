//! Inline runs and link targets shared by the document parsers.

use crate::domain::error::DomainError;
use crate::domain::ir::{Inline, InlineStyle};
use crate::infra::parsers::limits::{BLOCK_COST, TextBudget};

/// Longest link target or image source kept; longer ones (inline data) drop.
pub const MAX_URL_LEN: usize = 2048;

/// A link target worth keeping: relative, or with a scheme readers can
/// follow (`http`, `https`, `mailto`, `ftp`, `tel`). Browsers ignore tabs and
/// line breaks in URLs, so those are removed before the scheme is judged.
pub fn link_target(href: &str) -> Option<String> {
    // Bound the work by the kept length before copying anything.
    let kept = href
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .take(MAX_URL_LEN + 1)
        .map(char::len_utf8)
        .sum::<usize>();
    if kept > MAX_URL_LEN {
        return None;
    }
    let href: String = href
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let href = href.trim_matches(|c: char| c <= ' ');
    // A consumer trimming Unicode whitespace would see the scheme behind it.
    if href.is_empty()
        || href.len() > MAX_URL_LEN
        || href.starts_with(char::is_whitespace)
        || href.chars().any(char::is_control)
    {
        return None;
    }
    let scheme = href.split_once(':').map(|(scheme, _)| scheme).filter(|s| {
        s.starts_with(|c: char| c.is_ascii_alphabetic())
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    });
    match scheme {
        None => Some(href.to_owned()),
        Some(s)
            if ["http", "https", "mailto", "ftp", "tel"]
                .iter()
                .any(|ok| s.eq_ignore_ascii_case(ok)) =>
        {
            Some(href.to_owned())
        }
        Some(_) => None,
    }
}

/// What [`Inlines::take`] trims from both ends.
const BLANK: [char; 3] = [' ', '\n', '\t'];

/// Inline runs being collected for one paragraph or heading.
#[derive(Default)]
pub struct Inlines {
    runs: Vec<Inline>,
}

impl Inlines {
    /// Appends `text`, charging it to `budget`, and also a new run
    /// ([`BLOCK_COST`]) with its copy of the link target: one long target
    /// over many differently styled runs would otherwise be stored once per
    /// run for free.
    pub(crate) fn push(
        &mut self,
        text: &str,
        style: &InlineStyle,
        link: Option<&str>,
        budget: &mut TextBudget,
    ) -> Result<(), DomainError> {
        budget.charge(text.len())?;
        let added = self.append(text, style, link);
        budget.charge(added)
    }

    /// Appends plain `text` that the caller has already charged to the text
    /// budget (PDF lines and their runs are charged as they are built).
    pub fn push_charged_text(&mut self, text: &str, style: &InlineStyle) {
        self.append(text, style, None);
    }

    /// Appends `text`; returns what a new run costs beyond the text: its
    /// [`BLOCK_COST`] and the copy of the link target.
    fn append(&mut self, text: &str, style: &InlineStyle, link: Option<&str>) -> usize {
        if text.is_empty() {
            return 0;
        }
        // Merge with the previous run when nothing but the text differs.
        match (self.runs.last_mut(), link) {
            (
                Some(Inline::Text {
                    text: prev,
                    style: s,
                }),
                None,
            ) if s == style => {
                prev.push_str(text);
                0
            }
            (
                Some(Inline::Link {
                    text: prev,
                    target,
                    style: s,
                }),
                Some(l),
            ) if s == style && target == l => {
                prev.push_str(text);
                0
            }
            _ => {
                let (run, added) = match link {
                    Some(target) => (
                        Inline::Link {
                            text: text.to_owned(),
                            target: target.to_owned(),
                            style: style.clone(),
                        },
                        target.len(),
                    ),
                    None => (
                        Inline::Text {
                            text: text.to_owned(),
                            style: style.clone(),
                        },
                        0,
                    ),
                };
                self.runs.push(run);
                added.saturating_add(BLOCK_COST)
            }
        }
    }

    /// Whether nothing has been pushed.
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    /// Whether the last character pushed is a space or line break (or
    /// nothing has been pushed), so a following space is redundant.
    pub fn ends_with_space(&self) -> bool {
        self.runs.last().is_none_or(|run| {
            let (Inline::Text { text, .. } | Inline::Link { text, .. } | Inline::Code { text, .. }) =
                run;
            text.ends_with([' ', '\n'])
        })
    }

    /// The text collected so far, untrimmed.
    pub fn text(&self) -> String {
        self.runs
            .iter()
            .map(|run| match run {
                Inline::Text { text, .. }
                | Inline::Link { text, .. }
                | Inline::Code { text, .. } => text.as_str(),
            })
            .collect()
    }

    /// Removes a hyphen that ends the text right after a letter (a word
    /// broken across lines); returns whether it did.
    pub fn drop_trailing_hyphen(&mut self) -> bool {
        let Some(last) = self.runs.last_mut() else {
            return false;
        };
        let text = run_text(last);
        let mut tail = text.chars().rev();
        let broken = tail.next().is_some_and(|c| matches!(c, '-' | '\u{2010}'))
            && tail.next().is_some_and(char::is_alphabetic);
        if broken {
            text.pop();
        }
        broken
    }

    /// The runs with leading and trailing whitespace removed; empty if
    /// nothing but whitespace was collected.
    pub fn take(&mut self) -> Vec<Inline> {
        let mut runs = std::mem::take(&mut self.runs);
        let mut start = 0;
        // Trimmed in place: the edge runs can hold a whole paragraph.
        while let Some(run) = runs.get_mut(start) {
            let text = run_text(run);
            let blank = text.len() - text.trim_start_matches(BLANK).len();
            text.drain(..blank);
            if !text.is_empty() {
                break;
            }
            start += 1;
        }
        runs.drain(..start);
        while let Some(last) = runs.last_mut() {
            let text = run_text(last);
            let kept = text.trim_end_matches(BLANK).len();
            text.truncate(kept);
            if !text.is_empty() {
                break;
            }
            runs.pop();
        }
        runs
    }
}

pub fn run_text(run: &mut Inline) -> &mut String {
    match run {
        Inline::Text { text, .. } | Inline::Link { text, .. } | Inline::Code { text, .. } => text,
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn each_new_run_costs_a_block() {
        let limits = crate::infra::parsers::limits::ParseLimits {
            max_text_bytes: 4 + 3 * BLOCK_COST,
            ..Default::default()
        };
        let bold = InlineStyle {
            bold: true,
            ..InlineStyle::default()
        };
        let plain = InlineStyle::default();
        let mut budget = TextBudget::new(&limits);
        let mut inlines = Inlines::default();
        // Three runs; the fourth push merges into the third.
        for (text, style) in [("a", &plain), ("b", &bold), ("c", &plain), ("d", &plain)] {
            inlines.push(text, style, None, &mut budget).unwrap();
        }
        assert_eq!(inlines.take().len(), 3);
        let err = inlines.push("e", &bold, None, &mut budget).unwrap_err();
        assert!(err.to_string().contains("max_text_bytes"), "{err}");
    }

    #[test]
    fn safe_schemes_and_relative_targets_are_kept() {
        for href in [
            "https://example.com/a?b=1",
            "HTTP://EXAMPLE.COM",
            "mailto:a@example.com",
            "ftp://example.com/f",
            "tel:+123",
            "/relative/path",
            "#anchor",
            "page.html",
        ] {
            assert_eq!(link_target(href).as_deref(), Some(href), "{href}");
        }
        assert_eq!(
            link_target("https://exa\nmple.com/\ta").as_deref(),
            Some("https://example.com/a"),
            "tabs and line breaks are removed, as browsers do"
        );
        assert_eq!(
            link_target("  https://example.com  ").as_deref(),
            Some("https://example.com"),
            "ASCII blanks around the target are trimmed"
        );
    }

    #[test]
    fn script_schemes_are_dropped_however_they_are_hidden() {
        for href in [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            // Browsers drop tabs and line breaks, so these read as the scheme.
            "j\ta\nvascript:alert(1)",
            "javascript\r:alert(1)",
            "vbscript:x",
            "data:text/html,x",
        ] {
            assert_eq!(link_target(href), None, "{href:?}");
        }
    }

    #[test]
    fn unicode_whitespace_and_control_characters_are_refused() {
        // A consumer that trims Unicode whitespace would see the scheme
        // behind it; a control character makes the target ambiguous.
        for href in [
            "\u{a0}javascript:alert(1)",
            "\u{2028}javascript:alert(1)",
            "\u{3000}https://example.com",
            "java\u{0}script:alert(1)",
            "https://example.com/\u{7f}",
            "https://example.com/\u{85}",
        ] {
            assert_eq!(link_target(href), None, "{href:?}");
        }
    }

    #[test]
    fn length_is_counted_in_bytes_after_removing_tabs_and_breaks() {
        let at_limit = format!("/{}", "a".repeat(MAX_URL_LEN - 1));
        assert_eq!(link_target(&at_limit).as_deref(), Some(at_limit.as_str()));
        assert_eq!(link_target(&format!("{at_limit}a")), None);
        // Multibyte characters count by their bytes, not as one each.
        let wide = format!("/{}", "\u{e9}".repeat(MAX_URL_LEN - 1));
        assert_eq!(link_target(&wide), None);
        let broken = format!("/{}", "a\n".repeat(MAX_URL_LEN - 1));
        assert_eq!(link_target(&broken).map(|t| t.len()), Some(MAX_URL_LEN));
    }
}
