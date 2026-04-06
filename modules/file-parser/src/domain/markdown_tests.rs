use super::*;
use crate::domain::ir::{
    Inline, InlineStyle, ParsedMetadata, ParsedSource, TableBlock, TableCell, TableRow,
};

#[test]
fn test_render_heading() {
    let doc = ParsedDocument {
        id: None,
        title: None,
        language: None,
        meta: ParsedMetadata {
            source: ParsedSource::LocalPath("test.txt".to_owned()),
            original_filename: None,
            content_type: None,
            created_at: None,
            modified_at: None,
            is_stub: false,
        },
        blocks: vec![
            ParsedBlock::Heading {
                level: 1,
                inlines: vec![Inline::plain("Title")],
            },
            ParsedBlock::Heading {
                level: 2,
                inlines: vec![Inline::plain("Subtitle")],
            },
        ],
    };

    let markdown = MarkdownRenderer::render(&doc);
    assert!(markdown.contains("# Title\n"));
    assert!(markdown.contains("## Subtitle\n"));
}

#[test]
fn test_render_paragraph() {
    let doc = ParsedDocument {
        id: None,
        title: None,
        language: None,
        meta: ParsedMetadata {
            source: ParsedSource::LocalPath("test.txt".to_owned()),
            original_filename: None,
            content_type: None,
            created_at: None,
            modified_at: None,
            is_stub: false,
        },
        blocks: vec![ParsedBlock::Paragraph {
            inlines: vec![Inline::plain("Hello world")],
        }],
    };

    let markdown = MarkdownRenderer::render(&doc);
    assert!(markdown.contains("Hello world\n"));
}

#[test]
fn test_render_styled_text() {
    let style = InlineStyle {
        bold: true,
        italic: true,
        ..Default::default()
    };

    let doc = ParsedDocument {
        id: None,
        title: None,
        language: None,
        meta: ParsedMetadata {
            source: ParsedSource::LocalPath("test.txt".to_owned()),
            original_filename: None,
            content_type: None,
            created_at: None,
            modified_at: None,
            is_stub: false,
        },
        blocks: vec![ParsedBlock::Paragraph {
            inlines: vec![Inline::styled("Bold and italic", style)],
        }],
    };

    let markdown = MarkdownRenderer::render(&doc);
    assert!(markdown.contains("**") && markdown.contains('*'));
}

#[test]
fn test_render_list() {
    let doc = ParsedDocument {
        id: None,
        title: None,
        language: None,
        meta: ParsedMetadata {
            source: ParsedSource::LocalPath("test.txt".to_owned()),
            original_filename: None,
            content_type: None,
            created_at: None,
            modified_at: None,
            is_stub: false,
        },
        blocks: vec![
            ParsedBlock::ListItem {
                level: 0,
                ordered: false,
                blocks: vec![ParsedBlock::Paragraph {
                    inlines: vec![Inline::plain("Item 1")],
                }],
            },
            ParsedBlock::ListItem {
                level: 1,
                ordered: false,
                blocks: vec![ParsedBlock::Paragraph {
                    inlines: vec![Inline::plain("Nested item")],
                }],
            },
        ],
    };

    let markdown = MarkdownRenderer::render(&doc);
    assert!(markdown.contains("- Item 1\n"));
    assert!(markdown.contains("  - Nested item\n"));
}

#[test]
fn test_render_code_block() {
    let doc = ParsedDocument {
        id: None,
        title: None,
        language: None,
        meta: ParsedMetadata {
            source: ParsedSource::LocalPath("test.txt".to_owned()),
            original_filename: None,
            content_type: None,
            created_at: None,
            modified_at: None,
            is_stub: false,
        },
        blocks: vec![ParsedBlock::CodeBlock {
            language: Some("rust".to_owned()),
            code: "fn main() {\n    println!(\"Hello\");\n}".to_owned(),
        }],
    };

    let markdown = MarkdownRenderer::render(&doc);
    assert!(markdown.contains("```rust\n"));
    assert!(markdown.contains("fn main()"));
}

#[test]
fn test_render_simple_table() {
    let table = TableBlock {
        rows: vec![
            TableRow {
                is_header: true,
                cells: vec![
                    TableCell {
                        blocks: vec![ParsedBlock::Paragraph {
                            inlines: vec![Inline::plain("Name")],
                        }],
                    },
                    TableCell {
                        blocks: vec![ParsedBlock::Paragraph {
                            inlines: vec![Inline::plain("Age")],
                        }],
                    },
                ],
            },
            TableRow {
                is_header: false,
                cells: vec![
                    TableCell {
                        blocks: vec![ParsedBlock::Paragraph {
                            inlines: vec![Inline::plain("Alice")],
                        }],
                    },
                    TableCell {
                        blocks: vec![ParsedBlock::Paragraph {
                            inlines: vec![Inline::plain("30")],
                        }],
                    },
                ],
            },
        ],
    };

    let doc = ParsedDocument {
        id: None,
        title: None,
        language: None,
        meta: ParsedMetadata {
            source: ParsedSource::LocalPath("test.txt".to_owned()),
            original_filename: None,
            content_type: None,
            created_at: None,
            modified_at: None,
            is_stub: false,
        },
        blocks: vec![ParsedBlock::Table(table)],
    };

    let markdown = MarkdownRenderer::render(&doc);
    assert!(markdown.contains("| Name |"));
    assert!(markdown.contains("| Age |"));
    assert!(markdown.contains("| --- |"));
    assert!(markdown.contains("| Alice |"));
}

#[test]
fn test_render_table_with_escaped_content() {
    let table = TableBlock {
        rows: vec![
            TableRow {
                is_header: true,
                cells: vec![TableCell {
                    blocks: vec![ParsedBlock::Paragraph {
                        inlines: vec![Inline::plain("Column")],
                    }],
                }],
            },
            TableRow {
                is_header: false,
                cells: vec![TableCell {
                    blocks: vec![ParsedBlock::Paragraph {
                        inlines: vec![Inline::plain("Pipe|test")],
                    }],
                }],
            },
            TableRow {
                is_header: false,
                cells: vec![TableCell {
                    blocks: vec![ParsedBlock::Paragraph {
                        inlines: vec![Inline::plain("Backslash\\test")],
                    }],
                }],
            },
        ],
    };

    let doc = ParsedDocument {
        id: None,
        title: None,
        language: None,
        meta: ParsedMetadata {
            source: ParsedSource::LocalPath("test.txt".to_owned()),
            original_filename: None,
            content_type: None,
            created_at: None,
            modified_at: None,
            is_stub: false,
        },
        blocks: vec![ParsedBlock::Table(table)],
    };

    let markdown = MarkdownRenderer::render(&doc);
    // Pipes and backslashes should be escaped
    assert!(markdown.contains("Pipe\\|test"));
    assert!(markdown.contains("Backslash\\\\test"));
}

#[test]
fn test_render_nested_table() {
    let inner_table = TableBlock {
        rows: vec![
            TableRow {
                is_header: true,
                cells: vec![TableCell {
                    blocks: vec![ParsedBlock::Paragraph {
                        inlines: vec![Inline::plain("Inner")],
                    }],
                }],
            },
            TableRow {
                is_header: false,
                cells: vec![TableCell {
                    blocks: vec![ParsedBlock::Paragraph {
                        inlines: vec![Inline::plain("Data")],
                    }],
                }],
            },
        ],
    };

    let outer_table = TableBlock {
        rows: vec![
            TableRow {
                is_header: true,
                cells: vec![TableCell {
                    blocks: vec![ParsedBlock::Paragraph {
                        inlines: vec![Inline::plain("Outer")],
                    }],
                }],
            },
            TableRow {
                is_header: false,
                cells: vec![TableCell {
                    blocks: vec![ParsedBlock::Table(inner_table)],
                }],
            },
        ],
    };

    let doc = ParsedDocument {
        id: None,
        title: None,
        language: None,
        meta: ParsedMetadata {
            source: ParsedSource::LocalPath("test.txt".to_owned()),
            original_filename: None,
            content_type: None,
            created_at: None,
            modified_at: None,
            is_stub: false,
        },
        blocks: vec![ParsedBlock::Table(outer_table)],
    };

    let markdown = MarkdownRenderer::render(&doc);
    // Should contain both tables rendered
    assert!(markdown.contains("Outer"));
    assert!(markdown.contains("Inner"));
}

#[test]
fn test_render_with_title() {
    let doc = ParsedDocument {
        id: None,
        title: Some("Document Title".to_owned()),
        language: None,
        meta: ParsedMetadata {
            source: ParsedSource::LocalPath("test.txt".to_owned()),
            original_filename: None,
            content_type: None,
            created_at: None,
            modified_at: None,
            is_stub: false,
        },
        blocks: vec![ParsedBlock::Paragraph {
            inlines: vec![Inline::plain("Content")],
        }],
    };

    let markdown = MarkdownRenderer::render(&doc);
    assert!(markdown.starts_with("# Document Title\n"));
}

#[test]
fn test_render_iter_streaming() {
    let doc = ParsedDocument {
        id: None,
        title: Some("Test Title".to_owned()),
        language: Some("en".to_owned()),
        meta: ParsedMetadata {
            source: ParsedSource::LocalPath("test.txt".to_owned()),
            original_filename: Some("test.txt".to_owned()),
            content_type: Some("text/plain".to_owned()),
            created_at: None,
            modified_at: None,
            is_stub: false,
        },
        blocks: vec![
            ParsedBlock::Heading {
                level: 2,
                inlines: vec![Inline::plain("Section 1")],
            },
            ParsedBlock::Paragraph {
                inlines: vec![Inline::plain("First paragraph")],
            },
            ParsedBlock::Paragraph {
                inlines: vec![Inline::plain("Second paragraph")],
            },
        ],
    };

    // Collect chunks from iterator using render_iter_ref
    let chunks: Vec<String> = MarkdownRenderer::render_iter_ref(&doc).collect();

    // Should have header + 3 blocks = 4 chunks
    assert_eq!(chunks.len(), 4);

    // First chunk is header with title and metadata
    assert!(chunks[0].contains("# Test Title"));
    assert!(chunks[0].contains("language: en"));
    assert!(chunks[0].contains("filename: test.txt"));
    assert!(chunks[0].contains("content-type: text/plain"));

    // Remaining chunks are blocks
    assert!(chunks[1].contains("## Section 1"));
    assert!(chunks[2].contains("First paragraph"));
    assert!(chunks[3].contains("Second paragraph"));

    // Streamed result should match non-streamed
    let streamed = chunks.join("");
    let non_streamed = MarkdownRenderer::render(&doc);
    assert_eq!(streamed, non_streamed);
}

#[test]
fn test_render_iter_no_header() {
    let doc = ParsedDocument {
        id: None,
        title: None,
        language: None,
        meta: ParsedMetadata {
            source: ParsedSource::LocalPath("test.txt".to_owned()),
            original_filename: None,
            content_type: None,
            created_at: None,
            modified_at: None,
            is_stub: false,
        },
        blocks: vec![ParsedBlock::Paragraph {
            inlines: vec![Inline::plain("Only content")],
        }],
    };

    let chunks: Vec<String> = MarkdownRenderer::render_iter_ref(&doc).collect();

    // Should have only 1 chunk (the paragraph, no header)
    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].contains("Only content"));
}
