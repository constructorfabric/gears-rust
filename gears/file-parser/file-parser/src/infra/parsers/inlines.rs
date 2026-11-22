//! Inline runs and link targets shared by the document parsers.

use crate::domain::ir::{Inline, InlineStyle};

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
        || href.starts_with(|c: char| c.is_whitespace() || c.is_control())
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

/// Inline runs being collected for one paragraph or heading.
#[derive(Default)]
pub struct Inlines {
    runs: Vec<Inline>,
}

impl Inlines {
    pub fn push(&mut self, text: &str, style: &InlineStyle, link: Option<&str>) {
        if text.is_empty() {
            return;
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
            }
            (
                Some(Inline::Link {
                    text: prev,
                    target,
                    style: s,
                }),
                Some(l),
            ) if s == style && target == l => prev.push_str(text),
            _ => self.runs.push(match link {
                Some(target) => Inline::Link {
                    text: text.to_owned(),
                    target: target.to_owned(),
                    style: style.clone(),
                },
                None => Inline::Text {
                    text: text.to_owned(),
                    style: style.clone(),
                },
            }),
        }
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

    /// The runs with leading and trailing whitespace removed; empty if
    /// nothing but whitespace was collected.
    pub fn take(&mut self) -> Vec<Inline> {
        let mut runs = std::mem::take(&mut self.runs);
        let mut start = 0;
        while let Some(run) = runs.get_mut(start) {
            let text = run_text(run);
            let trimmed = text.trim_start_matches([' ', '\n']).to_owned();
            if trimmed.is_empty() {
                start += 1;
            } else {
                *text = trimmed;
                break;
            }
        }
        runs.drain(..start);
        while let Some(last) = runs.last_mut() {
            let text = run_text(last);
            let trimmed = text.trim_end_matches([' ', '\n']).to_owned();
            if trimmed.is_empty() {
                runs.pop();
            } else {
                *text = trimmed;
                break;
            }
        }
        runs
    }
}

pub fn run_text(run: &mut Inline) -> &mut String {
    match run {
        Inline::Text { text, .. } | Inline::Link { text, .. } | Inline::Code { text, .. } => text,
    }
}
