use async_trait::async_trait;
use std::borrow::Cow;
use std::path::Path;

use crate::domain::error::DomainError;
use crate::domain::ir::{DocumentBuilder, ParsedBlock, ParsedSource};
use crate::domain::parser::FileParserBackend;

use super::backend::{DEFAULT_MAX_BYTES, run_blocking};
use super::bounded_read::read_bounded;
use super::limits::{BLOCK_COST, ParseLimits, TextBudget};

/// Reads `.txt`, `.log` and `.md` files as verbatim code blocks.
pub struct PlainTextParser {
    max_bytes: u64,
    limits: ParseLimits,
}

impl PlainTextParser {
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            limits: ParseLimits::default(),
        }
    }

    /// Replaces the default [`ParseLimits`]; only `max_text_bytes` applies.
    #[must_use]
    pub fn with_limits(mut self, limits: ParseLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Caps how much this parser reads from a local file into memory. The
    /// service's up-front `metadata()` check is not atomic against the file
    /// growing; this is what actually bounds the allocation.
    #[must_use]
    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }
}

impl Default for PlainTextParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl FileParserBackend for PlainTextParser {
    fn id(&self) -> &'static str {
        "plain_text"
    }

    fn supported_extensions(&self) -> &'static [&'static str] {
        &["txt", "log", "md"]
    }

    async fn parse_local_path(
        &self,
        path: &Path,
        _resolved_content_type: Option<&str>,
    ) -> Result<crate::domain::ir::ParsedDocument, DomainError> {
        let content = read_bounded(path, self.max_bytes).await?;
        let language = language_for(path.file_name().and_then(|s| s.to_str()));
        let blocks = read_blocks(content, language, &self.limits).await?;

        let mut builder = DocumentBuilder::new(ParsedSource::LocalPath(path.display().to_string()))
            .content_type("text/plain")
            .blocks(blocks);

        if let Some(filename) = path.file_name().and_then(|s| s.to_str()) {
            builder = builder.title(filename).original_filename(filename);
        }

        Ok(builder.build())
    }

    async fn parse_bytes(
        &self,
        filename_hint: Option<&str>,
        _content_type: Option<&str>,
        bytes: bytes::Bytes,
    ) -> Result<crate::domain::ir::ParsedDocument, DomainError> {
        let blocks =
            read_blocks(Vec::from(bytes), language_for(filename_hint), &self.limits).await?;

        let source = ParsedSource::Uploaded {
            original_name: filename_hint.unwrap_or("unknown.txt").to_owned(),
        };

        let mut builder = DocumentBuilder::new(source)
            .content_type("text/plain")
            .blocks(blocks);

        if let Some(filename) = filename_hint {
            builder = builder.title(filename).original_filename(filename);
        }

        Ok(builder.build())
    }
}

/// Decodes `content` and splits it into blocks on the blocking pool, like
/// the other parsers: a large file takes a while to check and copy.
async fn read_blocks(
    content: Vec<u8>,
    language: Option<&'static str>,
    limits: &ParseLimits,
) -> Result<Vec<ParsedBlock>, DomainError> {
    let limits = limits.clone();
    run_blocking("plain text", move || {
        let text = String::from_utf8(content)
            .map_err(|e| DomainError::parse_error(format!("Failed to decode UTF-8: {e}")))?;
        text_to_blocks(&text, language, &limits)
    })
    .await
}

/// Upper bound for the text of one code block. Blank-line-separated
/// fragments are merged into a block up to this size.
const CODE_BLOCK_BYTES: usize = 16 * 1024;

/// A line longer than [`CODE_BLOCK_BYTES`] is kept whole up to this size;
/// past it the line is cut at a char boundary.
const MAX_LINE_BYTES: usize = 4 * CODE_BLOCK_BYTES;

/// Converts plain text into verbatim code blocks, so nothing in it is read
/// as Markdown or HTML. `\r\n` becomes `\n`; everything else is kept as is.
/// Text that fits [`CODE_BLOCK_BYTES`] is one block. Longer text is cut at
/// the last blank line that keeps the block within the cap, else at the last
/// line break, else at the first line break after the cap, else at
/// [`MAX_LINE_BYTES`]. Blank lines at a cut stay at the end of the block
/// before it, past the cap, so the blocks put together give the text back.
/// Each block is charged its text plus [`BLOCK_COST`]. Whitespace-only text
/// gives no blocks. `.md` files get the `markdown` language.
fn text_to_blocks(
    text: &str,
    language: Option<&str>,
    limits: &ParseLimits,
) -> Result<Vec<ParsedBlock>, DomainError> {
    let text: Cow<'_, str> = if text.contains("\r\n") {
        text.replace("\r\n", "\n").into()
    } else {
        text.into()
    };
    let mut budget = TextBudget::new(limits);
    let mut blocks = Vec::new();
    let mut rest = &*text;
    while !rest.is_empty() {
        let (code, next) = next_block(rest);
        rest = next;
        if code.trim().is_empty() {
            continue;
        }
        budget.charge(code.len().saturating_add(BLOCK_COST))?;
        blocks.push(ParsedBlock::CodeBlock {
            language: language.map(str::to_owned),
            code: code.to_owned(),
        });
    }
    Ok(blocks)
}

/// Splits `text` into the next block and the text after it; see
/// [`text_to_blocks`] for where it is cut. Only the first
/// [`MAX_LINE_BYTES`] of `text` are looked at to find the cut, so a long
/// line costs no more than its length over all the blocks.
fn next_block(text: &str) -> (&str, &str) {
    if text.len() <= CODE_BLOCK_BYTES {
        return (text, "");
    }
    // Ends of the lines that fit the cap: the last one, and the last one
    // followed by a blank line.
    let mut line_cut = None;
    let mut blank_cut = None;
    let mut start = 0;
    let mut prev_blank = true;
    for (i, _) in text.as_bytes()[..CODE_BLOCK_BYTES]
        .iter()
        .enumerate()
        .filter(|&(_, &b)| b == b'\n')
    {
        let blank = text[start..i].trim().is_empty();
        if blank && !prev_blank {
            blank_cut = Some(start);
        }
        start = i + 1;
        line_cut = Some(start);
        prev_blank = blank;
    }
    let cut = blank_cut.or(line_cut).unwrap_or_else(|| {
        // The first line alone is over the cap.
        let window = &text.as_bytes()[..text.len().min(MAX_LINE_BYTES + 1)];
        match window.iter().position(|&b| b == b'\n') {
            Some(i) => i + 1,
            None if text.len() <= MAX_LINE_BYTES => text.len(),
            None => {
                let mut cut = MAX_LINE_BYTES;
                while !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                cut
            }
        }
    });
    text.split_at(cut + blank_lines_len(&text[cut..]))
}

/// Length of the whitespace-only lines at the start of `text`, or of all of
/// it when it is whitespace only.
fn blank_lines_len(text: &str) -> usize {
    let blank = text.len() - text.trim_start().len();
    if blank == text.len() {
        blank
    } else {
        text[..blank].rfind('\n').map_or(0, |i| i + 1)
    }
}

/// The code block language for a file name: `markdown` for `.md`.
fn language_for(filename: Option<&str>) -> Option<&'static str> {
    let ext = Path::new(filename?).extension()?.to_str()?;
    ext.eq_ignore_ascii_case("md").then_some("markdown")
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::domain::markdown::MarkdownRenderer;

    fn blocks(text: &str) -> Vec<ParsedBlock> {
        text_to_blocks(text, None, &ParseLimits::default()).unwrap()
    }

    fn codes(text: &str) -> Vec<String> {
        blocks(text)
            .into_iter()
            .map(|b| match b {
                ParsedBlock::CodeBlock { code, .. } => code,
                other => panic!("not a code block: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_small_file_is_one_verbatim_block() {
        let text = "\n  indented\n\n\nnext \\[x] <b>\ntail";
        assert_eq!(
            blocks(text),
            [ParsedBlock::CodeBlock {
                language: None,
                code: text.to_owned(),
            }]
        );
        assert_eq!(codes("a\r\n\r\nb\r\n"), ["a\n\nb\n"]);
    }

    #[test]
    fn empty_and_blank_files_give_no_blocks() {
        assert!(blocks("").is_empty());
        assert!(blocks(" \n\n\t\n").is_empty());
    }

    #[tokio::test]
    async fn a_markdown_readme_renders_as_its_source() {
        let readme = "# Title\n\n````rust\nfn main() {}\n````\n\n```\n[link](javascript:x) ![i](p.png)\n\n<script>alert(1)</script>\n<!-- c -->\n";
        let doc = PlainTextParser::new()
            .parse_bytes(Some("README.md"), None, bytes::Bytes::from(readme))
            .await
            .unwrap();
        assert_eq!(
            doc.blocks,
            [ParsedBlock::CodeBlock {
                language: Some("markdown".to_owned()),
                code: readme.to_owned(),
            }]
        );
        let markdown = MarkdownRenderer::render(&doc);
        let body = format!("`````markdown\n{readme}`````\n\n");
        assert!(markdown.ends_with(&body), "{markdown}");
        assert_eq!(markdown.matches("`````").count(), 2, "{markdown}");
    }

    #[test]
    fn only_md_files_get_a_language() {
        assert_eq!(language_for(Some("a.MD")), Some("markdown"));
        assert_eq!(language_for(Some("a.log")), None);
        assert_eq!(language_for(Some("md")), None);
        assert_eq!(language_for(None), None);
    }

    #[test]
    fn long_text_is_cut_at_blank_lines_within_the_cap() {
        let para = format!("{}\n", "p".repeat(999));
        let fragment = para.repeat(3);
        let text = format!("{fragment}\n").repeat(12);
        let out = codes(&text);
        // Five 3000-byte fragments plus the four blank lines between them
        // fit 16 KiB; a sixth does not.
        // The blank line at each cut stays with the block before it.
        let five = format!("{fragment}\n").repeat(5);
        assert_eq!(
            out,
            [five.clone(), five, format!("{fragment}\n{fragment}\n")]
        );
        assert!(out.iter().all(|c| c.len() <= CODE_BLOCK_BYTES + 1));
        assert_eq!(out.concat(), text);
    }

    #[test]
    fn a_log_without_blank_lines_is_cut_at_line_breaks() {
        let line = format!("{}\n", "l".repeat(99));
        let text = line.repeat(400);
        let out = codes(&text);
        // 163 lines of 100 bytes fit 16 KiB.
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], line.repeat(163));
        assert_eq!(out[1], line.repeat(163));
        assert_eq!(out.concat(), text);
    }

    #[test]
    fn long_lines_are_kept_whole_up_to_the_hard_cap() {
        let long = "x".repeat(CODE_BLOCK_BYTES + 1);
        let text = format!("{long}\nshort\n");
        assert_eq!(codes(&text), [format!("{long}\n"), "short\n".to_owned()]);

        // 3-byte chars: the hard cap (65536) falls one byte into one, and
        // the cut backs off to its start.
        let huge = "\u{20ac}".repeat(MAX_LINE_BYTES.div_ceil(3));
        let out = codes(&huge);
        assert_eq!(out[0].len(), MAX_LINE_BYTES - 1);
        assert_eq!(out.concat(), huge);
    }

    #[test]
    fn each_block_is_charged_its_text() {
        let text = "a\n\nb";
        let fits = ParseLimits {
            max_text_bytes: text.len() + BLOCK_COST,
            ..ParseLimits::default()
        };
        assert_eq!(text_to_blocks(text, None, &fits).unwrap().len(), 1);
        let short = ParseLimits {
            max_text_bytes: text.len() + BLOCK_COST - 1,
            ..ParseLimits::default()
        };
        let err = text_to_blocks(text, None, &short).unwrap_err();
        assert!(err.to_string().contains("max_text_bytes"), "{err}");
    }

    #[tokio::test]
    async fn uploads_use_the_configured_limits() {
        let parser = PlainTextParser::new().with_limits(ParseLimits {
            max_text_bytes: BLOCK_COST,
            ..ParseLimits::default()
        });
        let err = parser
            .parse_bytes(Some("a.txt"), None, bytes::Bytes::from_static(b"a\n\nb"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("max_text_bytes"), "{err}");
    }

    #[test]
    fn the_blocks_put_together_give_the_text_back() {
        let para = format!("{}\n", "p".repeat(999));
        let text = format!(
            "\n  \n{}\n \n\t\n\n{}{}\n{}\n\n  \n",
            para.repeat(15),
            para.repeat(20),
            "x".repeat(MAX_LINE_BYTES + 10),
            "y".repeat(CODE_BLOCK_BYTES + 5),
        );
        let out = codes(&text);
        assert!(out.len() > 4, "{}", out.len());
        assert_eq!(out.concat(), text);
        // Text with no line break at all is cut at the hard cap only.
        let flat = "z".repeat(3 * MAX_LINE_BYTES + 1);
        let out = codes(&flat);
        assert_eq!(out.len(), 4);
        assert_eq!(out.concat(), flat);
    }
}
