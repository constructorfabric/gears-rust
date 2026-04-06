use modkit_macros::domain_model;

use crate::domain::ir::{ParsedBlock, ParsedDocument};

/// Markdown renderer that converts `ParsedDocument` to Markdown string
#[domain_model]
pub struct MarkdownRenderer;

impl Default for MarkdownRenderer {
    fn default() -> Self {
        Self::new()
    }
}

/// Iterator over Markdown chunks from a `ParsedDocument`
/// This iterator owns the document to avoid lifetime issues with async streaming
#[domain_model]
pub struct MarkdownRenderIter {
    doc: ParsedDocument,
    header_emitted: bool,
    block_index: usize,
}

impl Iterator for MarkdownRenderIter {
    type Item = String;

    fn next(&mut self) -> Option<Self::Item> {
        // First emit header chunk if not yet emitted
        if !self.header_emitted {
            self.header_emitted = true;
            let header = Self::render_header(&self.doc);
            if !header.is_empty() {
                return Some(header);
            }
        }

        // Then emit one chunk per block
        if self.block_index < self.doc.blocks.len() {
            let block = &self.doc.blocks[self.block_index];
            self.block_index += 1;
            let mut chunk = String::new();
            MarkdownRenderer::render_block(block, &mut chunk);
            Some(chunk)
        } else {
            None
        }
    }
}

impl MarkdownRenderIter {
    /// Render the header chunk (title + metadata)
    fn render_header(doc: &ParsedDocument) -> String {
        let mut header = String::new();

        // Render title if present
        if let Some(ref title) = doc.title {
            header.push_str("# ");
            header.push_str(title);
            header.push_str("\n\n");
        }

        // Render metadata section if we have useful info
        if doc.language.is_some()
            || doc.meta.original_filename.is_some()
            || doc.meta.content_type.is_some()
        {
            use std::fmt::Write;
            header.push_str("---\n");
            if let Some(ref lang) = doc.language {
                _ = writeln!(header, "language: {lang}");
            }
            if let Some(ref filename) = doc.meta.original_filename {
                _ = writeln!(header, "filename: {filename}");
            }
            if let Some(ref content_type) = doc.meta.content_type {
                _ = writeln!(header, "content-type: {content_type}");
            }
            header.push_str("---\n\n");
        }

        header
    }
}

impl MarkdownRenderer {
    /// Create a new markdown renderer
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Render a document using this renderer instance
    #[must_use]
    pub fn render_doc(doc: &ParsedDocument) -> String {
        Self::render(doc)
    }

    /// Create a streaming iterator over Markdown chunks
    /// Takes ownership of the document to avoid lifetime issues with async streaming
    #[must_use]
    pub fn render_iter(doc: ParsedDocument) -> MarkdownRenderIter {
        MarkdownRenderIter {
            doc,
            header_emitted: false,
            block_index: 0,
        }
    }

    /// Create a streaming iterator over Markdown chunks from a borrowed document
    /// This is a convenience method for when you don't need to move the document
    #[must_use]
    pub fn render_iter_ref(doc: &ParsedDocument) -> MarkdownRenderIter {
        MarkdownRenderIter {
            doc: doc.clone(),
            header_emitted: false,
            block_index: 0,
        }
    }

    /// Render a parsed document to Markdown (static method)
    /// Collects all chunks from the streaming iterator
    #[must_use]
    pub fn render(doc: &ParsedDocument) -> String {
        let mut output = String::new();
        for chunk in Self::render_iter_ref(doc) {
            output.push_str(&chunk);
        }
        output
    }

    fn render_block(block: &ParsedBlock, output: &mut String) {
        match block {
            ParsedBlock::Heading { level, inlines } => {
                let level = (*level).clamp(1, 6);
                output.push_str(&"#".repeat(level as usize));
                output.push(' ');
                Self::render_inlines(inlines, output);
                output.push_str("\n\n");
            }
            ParsedBlock::Paragraph { inlines } => {
                Self::render_inlines(inlines, output);
                output.push_str("\n\n");
            }
            ParsedBlock::ListItem {
                level,
                ordered,
                blocks,
            } => {
                // Add indentation
                let indent = "  ".repeat(*level as usize);
                output.push_str(&indent);

                // Add bullet or number
                if *ordered {
                    output.push_str("1. ");
                } else {
                    output.push_str("- ");
                }

                // Render blocks within list item
                for (idx, block) in blocks.iter().enumerate() {
                    if idx > 0 {
                        output.push_str(&indent);
                        output.push_str("   "); // Extra indent for continuation
                    }

                    let mut block_output = String::new();
                    Self::render_block(block, &mut block_output);
                    // Remove trailing double newlines from nested blocks
                    let block_text = block_output.trim_end();
                    output.push_str(block_text);

                    if idx < blocks.len() - 1 {
                        output.push('\n');
                    }
                }

                output.push('\n');
            }
            ParsedBlock::CodeBlock { language, code } => {
                output.push_str("```");
                if let Some(lang) = language {
                    output.push_str(lang);
                }
                output.push('\n');
                output.push_str(code);
                if !code.ends_with('\n') {
                    output.push('\n');
                }
                output.push_str("```\n\n");
            }
            ParsedBlock::Table(table_block) => {
                Self::render_table(table_block, output);
                output.push_str("\n\n");
            }
            ParsedBlock::Quote { blocks } => {
                let mut quote_content = String::new();
                for block in blocks {
                    Self::render_block(block, &mut quote_content);
                }

                // Prefix each line with "> "
                for line in quote_content.lines() {
                    output.push_str("> ");
                    output.push_str(line);
                    output.push('\n');
                }
                output.push('\n');
            }
            ParsedBlock::HorizontalRule => {
                output.push_str("---\n\n");
            }
            ParsedBlock::Image { alt, title, src } => {
                output.push('!');
                output.push('[');
                if let Some(alt_text) = alt {
                    output.push_str(alt_text);
                }
                output.push(']');
                output.push('(');
                if let Some(source) = src {
                    output.push_str(source);
                }
                if let Some(title_text) = title {
                    output.push_str(" \"");
                    output.push_str(title_text);
                    output.push('"');
                }
                output.push(')');
                output.push_str("\n\n");
            }
            ParsedBlock::PageBreak => {
                output.push_str("\n\n---\n\n");
            }
        }
    }

    fn render_inlines(inlines: &[crate::domain::ir::Inline], output: &mut String) {
        use crate::domain::ir::Inline;

        for inline in inlines {
            match inline {
                Inline::Text { text, style } => {
                    Self::render_styled_text(text, style, output);
                }
                Inline::Link {
                    text,
                    target,
                    style,
                } => {
                    output.push('[');
                    Self::render_styled_text(text, style, output);
                    output.push_str("](");
                    output.push_str(target);
                    output.push(')');
                }
                Inline::Code { text, style } => {
                    // Code inline takes precedence, then apply other styles
                    output.push('`');
                    Self::render_styled_text(text, style, output);
                    output.push('`');
                }
            }
        }
    }

    fn render_styled_text(text: &str, style: &crate::domain::ir::InlineStyle, output: &mut String) {
        let mut wrapped = text.to_owned();

        // Apply styles in order: code, bold, italic, underline, strike
        // Note: code is handled by Inline::Code variant, not here

        if style.strike {
            wrapped = format!("~~{wrapped}~~");
        }

        if style.underline {
            wrapped = format!("__{wrapped}__");
        }

        if style.italic {
            wrapped = format!("*{wrapped}*");
        }

        if style.bold {
            wrapped = format!("**{wrapped}**");
        }

        if style.code {
            wrapped = format!("`{wrapped}`");
        }

        output.push_str(&wrapped);
    }

    fn render_table(table: &crate::domain::ir::TableBlock, output: &mut String) {
        if table.rows.is_empty() {
            return;
        }

        // Determine number of columns from first row
        let num_cols = table.rows[0].cells.len();

        // Check if we have a header row
        let has_header = table.rows.first().is_some_and(|r| r.is_header);

        let (header_row, data_rows) = if has_header {
            (&table.rows[0], &table.rows[1..])
        } else {
            // Create synthetic header if needed
            let first_row = &table.rows[0];
            (first_row, &table.rows[..])
        };

        // Render header row
        Self::render_table_row(header_row, num_cols, output);
        output.push('\n');

        // Render separator row
        output.push('|');
        for _ in 0..num_cols {
            output.push_str(" --- |");
        }
        output.push('\n');

        // Render data rows
        for row in data_rows {
            Self::render_table_row(row, num_cols, output);
            output.push('\n');
        }
    }

    fn render_table_row(row: &crate::domain::ir::TableRow, num_cols: usize, output: &mut String) {
        output.push('|');

        for i in 0..num_cols {
            output.push(' ');

            if let Some(cell) = row.cells.get(i) {
                let cell_content = Self::render_cell_content(&cell.blocks);
                // Escape pipes and backslashes in cell content
                let escaped = Self::escape_table_content(&cell_content);
                output.push_str(&escaped);
            }

            output.push_str(" |");
        }
    }

    fn render_cell_content(blocks: &[ParsedBlock]) -> String {
        let mut content = String::new();

        for (idx, block) in blocks.iter().enumerate() {
            if idx > 0 {
                content.push_str("<br/>");
            }

            let mut block_output = String::new();
            Self::render_block(block, &mut block_output);

            // Remove trailing whitespace and newlines for cell content
            let trimmed = block_output.trim();
            content.push_str(trimmed);
        }

        content
    }

    fn escape_table_content(text: &str) -> String {
        text.replace('\\', "\\\\").replace('|', "\\|")
    }
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "markdown_tests.rs"]
mod tests;
