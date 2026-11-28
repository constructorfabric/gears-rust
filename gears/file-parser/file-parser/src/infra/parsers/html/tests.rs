use super::*;
use crate::infra::parsers::inlines::MAX_URL_LEN;
use crate::infra::parsers::limits::BLOCK_COST;

fn page(html: &str) -> Page {
    convert(html.as_bytes(), None, &ParseLimits::default()).expect("page converts")
}

fn blocks(html: &str) -> Vec<ParsedBlock> {
    page(html).blocks
}

fn plain(text: &str) -> Inline {
    Inline::plain(text)
}

fn styled(text: &str, f: impl FnOnce(&mut InlineStyle)) -> Inline {
    let mut style = InlineStyle::default();
    f(&mut style);
    Inline::Text {
        text: text.to_owned(),
        style,
    }
}

fn para(inlines: Vec<Inline>) -> ParsedBlock {
    ParsedBlock::Paragraph { inlines }
}

fn texts(blocks: &[ParsedBlock]) -> Vec<String> {
    fn inline_text(inlines: &[Inline]) -> String {
        inlines
            .iter()
            .map(|i| match i {
                Inline::Text { text, .. }
                | Inline::Link { text, .. }
                | Inline::Code { text, .. } => text.as_str(),
            })
            .collect()
    }
    blocks
        .iter()
        .filter_map(|b| match b {
            ParsedBlock::Paragraph { inlines } | ParsedBlock::Heading { inlines, .. } => {
                Some(inline_text(inlines))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn headings_once_with_title_and_language() {
    let p = page(
        r#"<!doctype html><html lang="en"><head><title> Sample
        Page </title><style>p{}</style></head>
        <body><h1>Main <em>Title</em></h1><p>Body</p></body></html>"#,
    );
    assert_eq!(p.title.as_deref(), Some("Sample Page"));
    assert_eq!(p.language.as_deref(), Some("en"));
    assert_eq!(
        p.blocks,
        [
            ParsedBlock::Heading {
                level: 1,
                inlines: vec![plain("Main "), styled("Title", |s| s.italic = true)],
            },
            para(vec![plain("Body")]),
        ]
    );
}

#[test]
fn nested_inline_styles_and_links() {
    let b =
        blocks(r#"<p>a <b>bold <a href="https://e.com/x"><i>link</i></a></b> <code>c</code></p>"#);
    assert_eq!(
        b,
        [para(vec![
            plain("a "),
            styled("bold ", |s| s.bold = true),
            Inline::Link {
                text: "link".to_owned(),
                target: "https://e.com/x".to_owned(),
                style: InlineStyle {
                    bold: true,
                    italic: true,
                    ..InlineStyle::default()
                },
            },
            plain(" "),
            styled("c", |s| s.code = true),
        ])]
    );
}

#[test]
fn unsafe_and_oversized_links_are_text() {
    let long = format!("https://e.com/{}", "a".repeat(MAX_URL_LEN));
    let b = blocks(&format!(
        r#"<p><a href="javascript:alert(1)">js</a> <a href="  ">empty</a> <a href="{long}">long</a></p>"#
    ));
    assert_eq!(b, [para(vec![plain("js empty long")])]);
}

#[test]
fn non_content_and_hidden_elements_are_skipped() {
    let b = blocks(
        r#"<body><script>var x;</script><noscript>no</noscript><template><p>t</p></template>
        <svg><text>s</text></svg><button>Click</button><select><option>o</option></select>
        <p hidden>h</p><div aria-hidden="true">a</div><span style="display: none">d</span>
        <p>kept</p></body>"#,
    );
    assert_eq!(texts(&b), ["kept"]);
}

#[test]
fn whitespace_collapses_except_in_pre() {
    let b = blocks(
        "<p>  one\n\n   two  <b> three </b>  </p><pre class=\"language-rust\">\n  fn  main() {}\n</pre>",
    );
    assert_eq!(
        b,
        [
            // Whitespace at the end of the paragraph is dropped.
            para(vec![plain("one two "), styled("three", |s| s.bold = true)]),
            ParsedBlock::CodeBlock {
                language: Some("rust".to_owned()),
                code: "  fn  main() {}".to_owned(),
            },
        ]
    );
}

#[test]
fn code_language_from_inner_code_element() {
    let b = blocks(r#"<pre><code class="lang-py">print(1)<br>x</code></pre>"#);
    assert_eq!(
        b,
        [ParsedBlock::CodeBlock {
            language: Some("py".to_owned()),
            code: "print(1)\nx".to_owned(),
        }]
    );
}

#[test]
fn nested_lists_keep_levels_and_order() {
    let b = blocks(
        "<ul><li>Apple</li><li>Banana<ul><li>Nested <b>one</b></li></ul></li></ul><ol><li><p>First</p></li><li>Second</li></ol>",
    );
    let items: Vec<(u8, bool, Vec<String>)> = b
        .iter()
        .map(|block| match block {
            ParsedBlock::ListItem {
                level,
                ordered,
                blocks,
            } => (*level, *ordered, texts(blocks)),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(
        items,
        [
            (0, false, vec!["Apple".to_owned()]),
            (0, false, vec!["Banana".to_owned()]),
            (1, false, vec!["Nested one".to_owned()]),
            (0, true, vec!["First".to_owned()]),
            (0, true, vec!["Second".to_owned()]),
        ]
    );
}

#[test]
fn definition_list() {
    let b = blocks("<dl><dt>Term</dt><dd>Meaning</dd></dl>");
    assert_eq!(
        b,
        [
            para(vec![styled("Term", |s| s.bold = true)]),
            para(vec![plain("Meaning")]),
        ]
    );
}

#[test]
fn table_with_header_spans_and_caption() {
    let b = blocks(
        r#"<table><caption>Parts</caption>
        <thead><tr><th>Name</th><th>Qty</th><th>Note</th></tr></thead>
        <tbody><tr><td rowspan="2">Bolt</td><td colspan="2">10 each</td></tr>
        <tr><td>5</td><td>spare</td></tr></tbody></table>"#,
    );
    assert_eq!(texts(&b[..1]), ["Parts"]);
    let ParsedBlock::Table(table) = &b[1] else {
        panic!("expected a table: {b:?}");
    };
    let rows: Vec<(bool, Vec<String>)> = table
        .rows
        .iter()
        .map(|r| {
            (
                r.is_header,
                r.cells.iter().flat_map(|c| texts(&c.blocks)).collect(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (true, vec!["Name".into(), "Qty".into(), "Note".into()]),
            (false, vec!["Bolt".into(), "10 each".into(), String::new()]),
            (false, vec![String::new(), "5".into(), "spare".into()]),
        ]
    );
}

#[test]
fn table_wider_than_the_column_limit_is_skipped_with_a_note() {
    let limits = ParseLimits {
        max_table_columns: 4,
        ..ParseLimits::default()
    };
    let row = "<td>x</td>".repeat(5);
    let html = format!("<table><tr>{row}</tr></table><p>after</p>");
    let t = texts(&convert(html.as_bytes(), None, &limits).unwrap().blocks);
    assert!(t[0].contains("Table skipped"), "{t:?}");
    assert_eq!(t[1], "after");
    let fits = format!("<table><tr>{}</tr></table>", "<td>x</td>".repeat(4));
    let b = convert(fits.as_bytes(), None, &limits).unwrap().blocks;
    assert!(matches!(b[0], ParsedBlock::Table(_)), "{b:?}");
}

#[test]
fn table_over_the_cell_budget_is_skipped_with_a_note() {
    let limits = ParseLimits {
        max_table_cells: 3,
        ..ParseLimits::default()
    };
    let html =
        "<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table><p>after</p>";
    let b = convert(html.as_bytes(), None, &limits).unwrap().blocks;
    let t = texts(&b);
    assert!(t[0].contains("Table skipped"), "{t:?}");
    assert_eq!(t[1], "after");
    // A huge colspan is clamped and still counted against the budget.
    let wide = r#"<table><tr><td colspan="999999">x</td></tr></table>"#;
    let b = convert(wide.as_bytes(), None, &limits).unwrap().blocks;
    assert!(texts(&b)[0].contains("Table skipped"));
}

#[test]
fn nested_tables_leave_room_for_the_cells_around_them() {
    // The outer table is charged only when it ends, so the table in its
    // second cell must leave room for the outer table's 7 cells: it gets 3
    // and is skipped, and the outer table fits.
    let limits = ParseLimits {
        max_table_cells: 10,
        ..ParseLimits::default()
    };
    let inner = r#"<table><tr><td colspan="6">b</td></tr></table>"#;
    let html = format!(r#"<table><tr><td colspan="6">a</td><td>{inner}</td></tr></table>"#);
    let b = convert(html.as_bytes(), None, &limits).unwrap().blocks;
    let [ParsedBlock::Table(table)] = b.as_slice() else {
        panic!("the outer table is kept: {b:?}");
    };
    let last = table.rows[0].cells.last().map(|c| texts(&c.blocks));
    assert!(
        last.is_some_and(|t| t.iter().any(|x| x.contains("Table skipped"))),
        "{table:?}"
    );
}

#[test]
fn a_table_in_a_spanning_cell_leaves_room_for_the_span() {
    // The enclosing cell spans 4 columns: the inner table gets 10 - 3 - 4.
    let limits = ParseLimits {
        max_table_cells: 10,
        ..ParseLimits::default()
    };
    let inner = r#"<table><tr><td colspan="5">b</td></tr></table>"#;
    let html =
        format!(r#"<table><tr><td colspan="3">a</td><td colspan="4">{inner}</td></tr></table>"#);
    let b = convert(html.as_bytes(), None, &limits).unwrap().blocks;
    let [ParsedBlock::Table(table)] = b.as_slice() else {
        panic!("the outer table is kept: {b:?}");
    };
    assert!(
        texts(&table.rows[0].cells[3].blocks)[0].contains("Table skipped"),
        "{table:?}"
    );
}

#[test]
fn a_table_directly_in_a_table_leaves_room_for_it() {
    // The inner table is converted after the outer grid is built but before
    // the outer table is charged: it must leave room for those 6 cells.
    let limits = ParseLimits {
        max_table_cells: 10,
        ..ParseLimits::default()
    };
    let html = concat!(
        r#"<table><tr><td colspan="6">a</td></tr>"#,
        r#"<table><tr><td colspan="6">b</td></tr></table></table>"#,
    );
    let b = convert(html.as_bytes(), None, &limits).unwrap().blocks;
    let [note, ParsedBlock::Table(_)] = b.as_slice() else {
        panic!("the inner table is skipped, the outer kept: {b:?}");
    };
    assert!(texts(std::slice::from_ref(note))[0].contains("Table skipped"));
}

#[test]
fn image_sources_and_blocks_are_charged() {
    let with = |max_text_bytes| ParseLimits {
        max_text_bytes,
        ..ParseLimits::default()
    };
    let src = format!("/{}", "s".repeat(1_999));
    let html = format!(r#"<img src="{src}" alt="a">"#);
    let cost = 1 + src.len() + BLOCK_COST;
    assert!(convert(html.as_bytes(), None, &with(cost)).is_ok());
    let err = convert(html.as_bytes(), None, &with(cost - 1))
        .err()
        .expect("the source is charged");
    assert!(err.to_string().contains("max_text_bytes"), "{err}");

    let rules = "<hr>".repeat(100);
    let b = convert(rules.as_bytes(), None, &with(100 * BLOCK_COST)).unwrap();
    assert_eq!(b.blocks.len(), 100);
    let err = convert(rules.as_bytes(), None, &with(100 * BLOCK_COST - 1))
        .err()
        .expect("every block is charged");
    assert!(err.to_string().contains("max_text_bytes"), "{err}");
}

#[test]
fn blockquote_br_hr_and_images() {
    let b = blocks(
        r#"<blockquote><p>Quoted</p></blockquote><p>Line one<br>Line two</p><hr>
        <img src="pic.png" alt="A picture" title="T"><img src="data:image/png;base64,AAAA" alt="inline">"#,
    );
    assert_eq!(
        b,
        [
            ParsedBlock::Quote {
                blocks: vec![para(vec![plain("Quoted")])]
            },
            para(vec![plain("Line one\nLine two")]),
            ParsedBlock::HorizontalRule,
            ParsedBlock::Image {
                alt: Some("A picture".to_owned()),
                title: Some("T".to_owned()),
                src: Some("pic.png".to_owned()),
            },
            ParsedBlock::Image {
                alt: Some("inline".to_owned()),
                title: None,
                src: None,
            },
        ]
    );
}

#[test]
fn loose_text_and_blocks_inside_inline_elements() {
    let b = blocks("<body>loose <span>text<div>block</div>after</span></body>");
    assert_eq!(texts(&b), ["loose text", "block", "after"]);
}

#[test]
fn nesting_past_max_depth_is_flattened_by_the_converter() {
    // Cheap enough to parse, deeper than the converter follows: the subtree
    // past MAX_DEPTH becomes text of the current paragraph.
    let depth = 1_000;
    let html = format!(
        "{}deep <b>text</b><p>more</p>{}",
        "<div>".repeat(depth),
        "</div>".repeat(depth)
    );
    assert_eq!(texts(&blocks(&html)), ["deep text more"]);
    let html = format!("{}x{}", "<b><i>".repeat(depth), "</i></b>".repeat(depth));
    assert_eq!(texts(&blocks(&html)), ["x"]);
    let html = format!(
        "{}x{}",
        "<ul><li>".repeat(depth),
        "</li></ul>".repeat(depth)
    );
    let b = blocks(&html);
    assert!(
        matches!(b.last(), Some(ParsedBlock::Paragraph { .. })),
        "the over-deep tail of the list is one flat paragraph: {:?}",
        b.last()
    );
}

#[test]
fn very_deep_nesting_stays_linear() {
    // 50 000 nested lists: a spec tree builder walks the open-element stack
    // per tag (minutes); the tree here stops at its depth cap and keeps the
    // text.
    let depth = 50_000;
    let html = format!(
        "{}deep <b>text</b><p>more</p>{}",
        "<ul><li>".repeat(depth),
        "</li></ul>".repeat(depth)
    );
    // A quadratic parse of this input would not finish in test time.
    assert_eq!(texts(&blocks(&html)), ["deep text more"]);
}

#[test]
fn text_limit_applies() {
    let limits = ParseLimits {
        max_text_bytes: 10,
        ..ParseLimits::default()
    };
    let err = convert(b"<p>more than ten bytes</p>", None, &limits)
        .err()
        .expect("must be rejected");
    assert!(err.to_string().contains("max_text_bytes"), "{err}");
}

#[test]
fn declared_charset_is_used() {
    let p = convert(
        b"<p>\xE4\xE0</p>",
        Some("text/html; charset=windows-1251"),
        &ParseLimits::default(),
    )
    .unwrap();
    assert_eq!(texts(&p.blocks), ["\u{434}\u{430}"]);
}

/// Text bytes and table cells in `blocks`, nested blocks included.
fn output_size(blocks: &[ParsedBlock]) -> (usize, usize) {
    fn inline_bytes(inlines: &[Inline]) -> usize {
        inlines
            .iter()
            .map(|i| match i {
                Inline::Text { text, .. } | Inline::Code { text, .. } => text.len(),
                Inline::Link { text, target, .. } => text.len() + target.len(),
            })
            .sum()
    }
    let (mut bytes, mut cells) = (0, 0);
    for block in blocks {
        let (b, c) = match block {
            ParsedBlock::Paragraph { inlines } | ParsedBlock::Heading { inlines, .. } => {
                (inline_bytes(inlines), 0)
            }
            ParsedBlock::ListItem { blocks, .. } | ParsedBlock::Quote { blocks } => {
                output_size(blocks)
            }
            ParsedBlock::CodeBlock { code, .. } => (code.len(), 0),
            ParsedBlock::Image { alt, title, src } => (
                [alt, title, src]
                    .iter()
                    .map(|v| v.as_ref().map_or(0, String::len))
                    .sum(),
                0,
            ),
            ParsedBlock::Table(table) => {
                let mut size = (0, 0);
                for cell in table.rows.iter().flat_map(|r| &r.cells) {
                    let (b, c) = output_size(&cell.blocks);
                    size = (size.0 + b, size.1 + c + 1);
                }
                size
            }
            ParsedBlock::HorizontalRule | ParsedBlock::PageBreak => (0, 0),
        };
        bytes += b;
        cells += c;
    }
    (bytes, cells)
}

/// Inputs that made a spec tree builder (html5ever) super-linear for
/// seconds to minutes; here each one is a single pass.
#[test]
fn hostile_markup_stays_linear() {
    let cases = [
        // Raw-text tags inside foreign content, then deep nesting.
        format!("<svg><plaintext>{}", "<div>".repeat(80_000)),
        // Many unmatched end tags under a deep stack.
        format!("{}{}", "<span>".repeat(18_000), "</zz>".repeat(400_000)),
        // End tags popping across many open blocks.
        "<span>".to_owned() + &"<div>".repeat(1_000) + &"</span>".repeat(100),
        // Misnested formatting elements with distinct attributes.
        (0..8_000).fold(String::new(), |mut acc, i| {
            use std::fmt::Write as _;
            write!(acc, "<p><b id={i}></p>").unwrap();
            acc
        }),
        // End tags that may not close across cells, under nested tables.
        format!(
            "{}{}",
            "<table><tr><td><span>".repeat(5_000),
            "</div></span></td></tr></table>".repeat(50_000)
        ),
        // One tag with a hundred thousand attributes.
        format!("<p{}>x", " a=1".repeat(100_000)),
    ];
    for html in &cases {
        let started = std::time::Instant::now();
        let page = convert(html.as_bytes(), None, &ParseLimits::default())
            .unwrap_or_else(|e| panic!("{e}: {}", &html[..80]));
        // Generous for a debug build; quadratic work takes minutes.
        assert!(started.elapsed().as_secs() < 10, "{}", &html[..80]);
        let (bytes, cells) = output_size(&page.blocks);
        assert!(bytes <= html.len(), "{bytes}");
        assert!(cells <= ParseLimits::default().max_table_cells, "{cells}");
    }
}

#[test]
fn rowspan_fill_counts_against_the_cell_budget() {
    // Spans covering 16 000 columns, then many one-cell rows: the filled grid
    // would be enormous; the table is skipped instead.
    let html = format!(
        "<table><tr>{}</tr>{}</table><p>after</p>",
        r#"<td colspan="1000" rowspan="65534">x</td>"#.repeat(16),
        "<tr><td>y</td></tr>".repeat(2_000)
    );
    let b = blocks(&html);
    let t = texts(&b);
    assert!(t[0].contains("Table skipped"), "{t:?}");
    assert_eq!(t[1], "after");
}

#[test]
fn node_limit_rejects_huge_documents() {
    let limits = ParseLimits {
        max_html_nodes: 1_000,
        ..ParseLimits::default()
    };
    let err = convert("<p>x".repeat(2_000).as_bytes(), None, &limits)
        .err()
        .expect("limit");
    assert!(err.to_string().contains("max_html_nodes"), "{err}");
}

#[test]
fn heading_keeps_document_order_and_image_alt() {
    let b = blocks(
        r#"<h1>A<div>B</div>C</h1><h2><a href="/"><img alt="Company" src="logo.png"></a></h2>"#,
    );
    assert_eq!(texts(&b), ["A B C", "Company"]);
    assert!(matches!(&b[1], ParsedBlock::Heading { inlines, .. }
        if matches!(&inlines[0], Inline::Link { target, .. } if target == "/")));
}

#[test]
fn script_links_with_hidden_whitespace_are_text() {
    let b = blocks(
        "<p><a href=\"java&#10;script:alert(1)\">a</a> <a href=\" \tvbscript:x\">b</a> \
         <a href=\"mailto:x@e.com\">c</a> <a href=\"/rel\">d</a></p>",
    );
    let ParsedBlock::Paragraph { inlines } = &b[0] else {
        panic!("{b:?}");
    };
    let links: Vec<&str> = inlines
        .iter()
        .filter_map(|i| match i {
            Inline::Link { target, .. } => Some(target.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(links, ["mailto:x@e.com", "/rel"]);
}

#[test]
fn list_directly_in_list_nests_and_loose_text_is_an_item() {
    let b = blocks("<ul><li>a</li><ul><li>b</li></ul>loose</ul>");
    let items: Vec<(u8, Vec<String>)> = b
        .iter()
        .map(|block| match block {
            ParsedBlock::ListItem { level, blocks, .. } => (*level, texts(blocks)),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(
        items,
        [
            (0, vec!["a".to_owned()]),
            (1, vec!["b".to_owned()]),
            (0, vec!["loose".to_owned()])
        ]
    );
}

#[test]
fn a_huge_title_is_cut() {
    let html = format!("<title>{}</title><p>x</p>", "\u{e9}".repeat(100_000));
    let p = page(&html);
    assert_eq!(
        p.title,
        Some("\u{e9}".repeat(crate::infra::parsers::limits::MAX_TITLE_CHARS))
    );
}

#[test]
fn title_outside_head_is_used() {
    let p = page("<body><title>Late</title><p>x</p></body>");
    assert_eq!(p.title.as_deref(), Some("Late"));
    assert_eq!(texts(&p.blocks), ["x"]);
}

#[test]
fn empty_row_uses_up_a_rowspan() {
    let b = blocks(
        r#"<table><tr><td rowspan="2">a</td><td>b</td></tr><tr></tr><tr><td>c</td><td>d</td></tr></table>"#,
    );
    let ParsedBlock::Table(table) = &b[0] else {
        panic!("{b:?}");
    };
    let last: Vec<String> = table
        .rows
        .last()
        .unwrap()
        .cells
        .iter()
        .flat_map(|c| texts(&c.blocks))
        .collect();
    assert_eq!(last, ["c", "d"], "the span ended with the empty row");
}

#[test]
fn flattened_subtrees_keep_words_apart() {
    let depth = 1_000;
    let html = format!(
        "{}<div>a</div>b<table><tr><td>c</td><td>d</td></tr></table>e<br>f{}",
        "<div>".repeat(depth),
        "</div>".repeat(depth)
    );
    assert_eq!(texts(&blocks(&html)), ["a b c d e f"]);
}

#[test]
fn body_after_an_unclosed_head_is_kept() {
    let p = page("<html><head><title>T</title><body><p>Hello</p>");
    assert_eq!(p.title.as_deref(), Some("T"));
    assert_eq!(texts(&p.blocks), ["Hello"]);
    let p = page("<head><meta charset=utf-8><title>T</title><p>Hello");
    assert_eq!(texts(&p.blocks), ["Hello"]);
}

fn first_table(b: &[ParsedBlock]) -> Vec<Vec<String>> {
    let table = b
        .iter()
        .find_map(|block| match block {
            ParsedBlock::Table(t) => Some(t),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no table in {b:?}"));
    table
        .rows
        .iter()
        .map(|r| r.cells.iter().flat_map(|c| texts(&c.blocks)).collect())
        .collect()
}

#[test]
fn legacy_table_markup_keeps_rows_and_stray_text() {
    let b = blocks("<table><form><tr><td>secret</td></tr></form></table>");
    assert_eq!(first_table(&b), [vec!["secret"]]);
    let b = blocks("<table><font size=2><tr><td>cell</td></tr></font></table>");
    assert_eq!(first_table(&b), [vec!["cell"]]);
    let b = blocks("<table>Total:<tr><td>9</td></tr></table>");
    assert_eq!(
        texts(&b[..1]),
        ["Total:"],
        "text outside cells precedes the table"
    );
    assert_eq!(first_table(&b), [vec!["9"]]);
}

#[test]
fn cells_close_through_unclosed_inline_elements() {
    let b = blocks("<table><tr><td><font>Name<td><font>Value<tr><td><font>A<td><font>B</table>");
    assert_eq!(first_table(&b), [vec!["Name", "Value"], vec!["A", "B"]]);
    // Stray cells outside a table are ignored but still separate words.
    assert_eq!(texts(&blocks("<div><td>a</td><td>b</td></div>")), ["a b"]);
}

#[test]
fn oversized_tables_spend_the_budget() {
    let limits = ParseLimits {
        max_table_cells: 1_000,
        ..ParseLimits::default()
    };
    let big = format!("<table><tr>{}</tr></table>", "<td>x</td>".repeat(900));
    // The first table, 2 x 600 cells, is over the budget and skipped; it
    // still spends the 600 cells it built, so the second, which would fit
    // alone, is skipped too.
    let html = format!(
        "<table><tr>{}</tr><tr>{}</tr></table>{big}",
        "<td>a</td>".repeat(600),
        "<td>b</td>".repeat(600)
    );
    let b = convert(html.as_bytes(), None, &limits).unwrap().blocks;
    let t = texts(&b);
    assert!(
        t.iter().filter(|x| x.contains("Table skipped")).count() == 2,
        "{t:?}"
    );
}

#[test]
fn attribute_query_strings_are_not_decoded_as_references() {
    let b = blocks(r#"<p><a href="/x?id=1&region=us&copy=2">q</a></p>"#);
    let ParsedBlock::Paragraph { inlines } = &b[0] else {
        panic!("{b:?}");
    };
    assert!(
        matches!(&inlines[0], Inline::Link { target, .. } if target == "/x?id=1&region=us&copy=2")
    );
}

#[test]
fn text_after_a_nested_list_continues_its_item() {
    let b = blocks("<ul><li>a<ul><li>b</ul>c</li></ul>");
    let items: Vec<(u8, Vec<String>)> = b
        .iter()
        .map(|block| match block {
            ParsedBlock::ListItem { level, blocks, .. } => (*level, texts(blocks)),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(
        items,
        [
            (0, vec!["a".to_owned()]),
            (1, vec!["b".to_owned()]),
            (0, vec!["c".to_owned()])
        ]
    );
}

#[test]
fn noframes_and_noembed_are_skipped() {
    let b = blocks("<noframes><body><p>No frames</p></noframes><noembed>x</noembed><p>kept</p>");
    assert_eq!(texts(&b), ["kept"]);
}

/// Random markup fragments: no panic, output bounded by the limits.
#[test]
fn random_markup_never_panics() {
    let limits = ParseLimits {
        max_table_cells: 40,
        max_html_nodes: 150,
        max_text_bytes: 4_096,
        ..ParseLimits::default()
    };
    let pieces: [&str; 24] = [
        "<",
        ">",
        "</",
        "<p>",
        "</p>",
        "<td>",
        "<tr>",
        "<table>",
        "</table>",
        "<ul><li>",
        "&",
        "&amp",
        "&#x",
        "<!--",
        "-->",
        "<script>",
        "</script>",
        "\"",
        "'",
        "=",
        "<b>",
        "\u{e9}",
        "<head>",
        " text ",
    ];
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..2_000 {
        let mut html = String::new();
        for _ in 0..64 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let index = usize::try_from(state % 24).unwrap();
            html.push_str(pieces[index]);
        }
        match convert(html.as_bytes(), None, &limits) {
            Ok(page) => {
                let (bytes, cells) = output_size(&page.blocks);
                assert!(bytes <= limits.max_text_bytes, "{bytes}: {html}");
                assert!(cells <= limits.max_table_cells, "{cells}: {html}");
            }
            Err(e) => assert!(
                ["max_text_bytes", "max_html_nodes"]
                    .iter()
                    .any(|l| e.to_string().contains(l)),
                "{e}: {html}"
            ),
        }
    }
}

#[test]
fn end_tags_do_not_close_across_cells() {
    let b = blocks("<font><table><tr><td>a</font></td><td>b</td></tr></table>");
    assert_eq!(first_table(&b), [vec!["a", "b"]]);
    let b = blocks(
        "<div><table><tr><td>a</div></td><td>b</td></tr><tr><td>c</td></tr></table>after</div>",
    );
    assert_eq!(first_table(&b), [vec!["a", "b"], vec!["c"]]);
    assert_eq!(texts(&b), ["after"]);
    // The end of a row or table still closes the cell around it.
    let b = blocks("<table><tr><td><b>a</tr><tr><td>b</table>c");
    assert_eq!(first_table(&b), [vec!["a"], vec!["b"]]);
    assert_eq!(texts(&b), ["c"]);
}

#[test]
fn unclosed_colgroup_and_caption_end_at_the_rows() {
    let b = blocks("<table><colgroup><col><col><tr><td>a</td></tr></table>");
    assert_eq!(first_table(&b), [vec!["a"]]);
    let b = blocks("<table><caption>Parts<tr><td>1</td></tr></table>");
    assert_eq!(texts(&b), ["Parts"]);
    assert_eq!(first_table(&b), [vec!["1"]]);
    // A table left in a colgroup is not skipped with the columns.
    let b = blocks("<table><colgroup><col><table><tr><td>in</table><tr><td>out</table>");
    let tables: Vec<Vec<Vec<String>>> = b
        .iter()
        .map(|block| first_table(std::slice::from_ref(block)))
        .collect();
    assert_eq!(tables, [vec![vec!["in"]], vec![vec!["out"]]]);
}

#[test]
fn blocks_inside_links_and_styles_keep_them() {
    let link = |text: &str, style: InlineStyle| Inline::Link {
        text: text.to_owned(),
        target: "/p".to_owned(),
        style,
    };
    let b = blocks(r#"<a href="/p"><h3>Title</h3><p>Summary</p></a>"#);
    assert_eq!(
        b,
        [
            ParsedBlock::Heading {
                level: 3,
                inlines: vec![link("Title", InlineStyle::default())],
            },
            para(vec![link("Summary", InlineStyle::default())]),
        ]
    );
    let b = blocks("<b><div><p>x</p></div><ul><li>y</li></ul></b>");
    assert_eq!(b[0], para(vec![styled("x", |s| s.bold = true)]));
    assert!(
        matches!(&b[1], ParsedBlock::ListItem { blocks, .. }
            if blocks[..] == [para(vec![styled("y", |s| s.bold = true)])]),
        "{b:?}"
    );
    let b = blocks("<i><dl><dt>t</dl></i>");
    assert_eq!(
        b,
        [para(vec![styled("t", |s| {
            s.bold = true;
            s.italic = true;
        })])]
    );
    // Cells and captions do not take the style or link around the table.
    let b = blocks(r#"<a href="/p"><b><table><caption>c</caption><tr><td>x</table></b></a>"#);
    assert_eq!(b[0], para(vec![plain("c")]));
    let ParsedBlock::Table(table) = &b[1] else {
        panic!("{b:?}");
    };
    assert_eq!(table.rows[0].cells[0].blocks, [para(vec![plain("x")])]);
}

#[test]
fn crlf_line_breaks_in_pre() {
    let b = blocks("<pre>\r\ncode\r\nmore\rlast\r\n</pre>");
    assert_eq!(
        b,
        [ParsedBlock::CodeBlock {
            language: None,
            code: "code\nmore\nlast".to_owned(),
        }]
    );
}

#[test]
fn hidden_cells_and_preformatted_text_are_skipped() {
    let b = blocks(
        r#"<table><tr><td style="display:none">secret</td><td>shown</td><th hidden>h</th></tr></table>"#,
    );
    assert_eq!(first_table(&b), [vec!["shown"]]);
    let b = blocks(r#"<pre>a<span style="visibility: hidden">secret</span>b</pre>"#);
    assert_eq!(
        b,
        [ParsedBlock::CodeBlock {
            language: None,
            code: "ab".to_owned(),
        }]
    );
}

#[test]
fn svg_and_math_titles_are_not_the_page_title() {
    let p = page("<body><svg><title>Icon</title></svg><math><title>m</title></math><p>x</p>");
    assert_eq!(p.title, None);
    let p = page("<svg><title>Icon</title></svg><title>Page</title>");
    assert_eq!(p.title.as_deref(), Some("Page"));
}

#[test]
fn script_text_in_comments_does_not_leak() {
    let b = blocks("<script><!-- document.write('<script></script>'); //--></script><p>kept</p>");
    assert_eq!(texts(&b), ["kept"]);
}

#[test]
fn table_structure_leftovers() {
    // A new cell closes a list left open in the cell before it.
    let b = blocks("<table><tr><td><ul><li>x<td>y<tr><td>z</table>");
    let ParsedBlock::Table(table) = &b[0] else {
        panic!("{b:?}");
    };
    assert_eq!(table.rows.len(), 2, "{table:?}");
    assert!(matches!(
        table.rows[0].cells[0].blocks[..],
        [ParsedBlock::ListItem { .. }]
    ));
    assert_eq!(texts(&table.rows[0].cells[1].blocks), ["y"]);
    assert_eq!(texts(&table.rows[1].cells[0].blocks), ["z"]);
    // Cells without a row share one.
    assert_eq!(
        first_table(&blocks("<table><td>a<td>b</table>")),
        [vec!["a", "b"]]
    );
    // A table in a row but outside its cells comes before the table.
    let b = blocks("<table><tr><table><tr><td>in</table><td>out</table>");
    let tables: Vec<Vec<Vec<String>>> = b
        .iter()
        .map(|block| first_table(std::slice::from_ref(block)))
        .collect();
    assert_eq!(tables, [vec![vec!["in"]], vec![vec!["out"]]]);
}

#[test]
fn cells_and_captions_outside_tables_are_ignored() {
    // Their content stays in the element around them, which still closes.
    for html in [
        "<select><option>a<td>b</select><p>after</p>",
        "<button><td>x</button><p>after</p>",
        "<button><caption>x</button><p>after</p>",
        "<button><colgroup>x</button><p>after</p>",
    ] {
        assert_eq!(texts(&blocks(html)), ["after"], "{html}");
    }
    assert_eq!(texts(&blocks("<div><td>Price</div>$5")), ["Price", "$5"]);
    let b = blocks("<ul><li>x<td>a</ul><p>after</p>");
    assert!(
        matches!(&b[..], [ParsedBlock::ListItem { blocks, .. }, last]
            if texts(blocks) == ["x a"] && texts(std::slice::from_ref(last)) == ["after"]),
        "{b:?}"
    );
}

#[test]
fn template_and_select_end_tags_close_tables_left_open_in_them() {
    for html in [
        "<template><table><tr><td>a</template><p>rest</p>",
        "<template><td>x</template><p>rest</p>",
        "<select><table><tr><td>a</select><p>rest</p>",
    ] {
        assert_eq!(texts(&blocks(html)), ["rest"], "{html}");
    }
}

#[test]
fn caption_after_rows_closes_them() {
    let b = blocks("<table><tr><td>a<caption>c</caption><tr><td>b</table>");
    assert_eq!(texts(&b), ["c"]);
    assert_eq!(first_table(&b), [vec!["a"], vec!["b"]]);
    let b = blocks("<table><tbody><tr><td>a<caption>c</caption>b</table>");
    assert_eq!(texts(&b), ["c", "b"]);
    assert_eq!(first_table(&b), [vec!["a"]]);
}

#[test]
fn self_closing_foreign_elements_are_closed() {
    let p = page("<svg/><title>T</title><p>x");
    assert_eq!(p.title.as_deref(), Some("T"));
    assert_eq!(texts(&p.blocks), ["x"]);
    assert_eq!(
        texts(&blocks(r#"<p>a<svg class="i"/> text</p>"#)),
        ["a text"]
    );
    assert_eq!(
        texts(&blocks("<p>a<math/>b<svg><path d=x/><title/></svg> c</p>")),
        ["ab c"]
    );
}
