use super::*;

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
        // One tag with a hundred thousand attributes.
        format!("<p{}>x", " a=1".repeat(100_000)),
    ];
    for html in &cases {
        drop(convert(html.as_bytes(), None, &ParseLimits::default()));
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
    // Stray cells outside a table still separate words.
    assert_eq!(
        texts(&blocks("<div><td>a</td><td>b</td></div>")),
        ["a", "b"]
    );
}

#[test]
fn oversized_tables_spend_the_budget() {
    let limits = ParseLimits {
        max_table_cells: 1_000,
        ..ParseLimits::default()
    };
    let big = format!("<table><tr>{}</tr></table>", "<td>x</td>".repeat(900));
    // The first table fits; the second would too, but the first oversized
    // table in between used up what it built.
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
        drop(convert(html.as_bytes(), None, &ParseLimits::default()));
    }
}
