#![expect(
    clippy::format_in_format_args,
    reason = "nested markup reads best inline"
)]

use std::fmt::Write as _;

use super::body::MAX_FIELDS;
use super::*;
use crate::domain::ir::{Inline, InlineStyle, TableBlock, TableCell, TableRow};
use crate::infra::parsers::limits::BLOCK_COST;
use crate::infra::parsers::ooxml::test_zip;

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing""#;
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

fn rels(items: &[(&str, &str, &str)]) -> String {
    let mut out = String::from(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    );
    for (id, kind, target) in items {
        let mode = if target.contains(':') {
            r#" TargetMode="External""#
        } else {
            ""
        };
        write!(
            out,
            r#"<Relationship Id="{id}" Type="{REL}/{kind}" Target="{target}"{mode}/>"#
        )
        .unwrap();
    }
    out.push_str("</Relationships>");
    out
}

const STYLES: &str = r#"<w:styles xmlns:w="w">
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
<w:style w:type="paragraph" w:styleId="827"><w:name w:val="Heading 1"/><w:basedOn w:val="Normal"/><w:pPr><w:outlineLvl w:val="0"/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="828"><w:name w:val="heading 2"/><w:basedOn w:val="Normal"/></w:style>
<w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/></w:style>
<w:style w:type="paragraph" w:styleId="Subtitle"><w:name w:val="Subtitle"/></w:style>
<w:style w:type="paragraph" w:styleId="Q"><w:name w:val="Quote"/></w:style>
<w:style w:type="paragraph" w:styleId="MyQuote"><w:name w:val="Mine"/><w:basedOn w:val="Q"/></w:style>
<w:style w:type="paragraph" w:styleId="Pre"><w:name w:val="Preformatted Text"/><w:basedOn w:val="Normal"/></w:style>
<w:style w:type="paragraph" w:styleId="ListBullet"><w:name w:val="List Bullet"/><w:pPr><w:numPr><w:numId w:val="1"/></w:numPr></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="Breaker"><w:name w:val="Breaker"/><w:pPr><w:pageBreakBefore/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="CycleA"><w:name w:val="A"/><w:basedOn w:val="CycleB"/></w:style>
<w:style w:type="paragraph" w:styleId="CycleB"><w:name w:val="B"/><w:basedOn w:val="CycleA"/></w:style>
<w:style w:type="character" w:styleId="Src"><w:name w:val="Source Text"/></w:style>
<w:style w:type="character" w:styleId="Strong"><w:name w:val="Strong"/><w:rPr><w:b/></w:rPr></w:style>
<w:style w:type="character" w:styleId="Hide"><w:name w:val="Hide"/><w:rPr><w:vanish/></w:rPr></w:style>
</w:styles>"#;

const NUMBERING: &str = r#"<w:numbering xmlns:w="w">
<w:abstractNum w:abstractNumId="10">
<w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl>
<w:lvl w:ilvl="1"><w:numFmt w:val="bullet"/><w:lvlText w:val="o"/></w:lvl>
<w:lvl w:ilvl="2"><w:numFmt w:val="none"/><w:lvlText w:val=""/></w:lvl>
<w:lvl w:ilvl="1000000"><w:numFmt w:val="bullet"/></w:lvl>
</w:abstractNum>
<w:abstractNum w:abstractNumId="11"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/></w:lvl><w:lvl w:ilvl="8"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum>
<w:num w:numId="1"><w:abstractNumId w:val="11"/></w:num>
<w:num w:numId="2"><w:abstractNumId w:val="10"/></w:num>
<w:num w:numId="3"><w:abstractNumId w:val="10"/><w:lvlOverride w:ilvl="0"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/></w:lvl></w:lvlOverride></w:num>
</w:numbering>"#;

/// A package around `body` (the inside of `w:body`), with the standard
/// styles and numbering and extra document relationships.
struct Package {
    parts: Vec<(String, Vec<u8>)>,
}

impl Package {
    fn new(body: &str) -> Self {
        Self::with_rels(body, &[])
    }

    fn with_rels(body: &str, extra: &[(&str, &str, &str)]) -> Self {
        let mut doc_rels = vec![
            ("rS", "styles", "styles.xml"),
            ("rW", "stylesWithEffects", "stylesWithEffects.xml"),
            ("rN", "numbering", "numbering.xml"),
        ];
        doc_rels.extend_from_slice(extra);
        let mut pkg = Self { parts: Vec::new() };
        pkg.set(
            "_rels/.rels",
            &rels(&[("rId1", "officeDocument", "word/document.xml")]),
        );
        pkg.set(
            "word/document.xml",
            &format!("<w:document {W}><w:body>{body}<w:sectPr/></w:body></w:document>"),
        );
        pkg.set("word/_rels/document.xml.rels", &rels(&doc_rels));
        pkg.set("word/styles.xml", STYLES);
        pkg.set(
            "word/stylesWithEffects.xml",
            r#"<w:styles xmlns:w="w"><w:style w:type="paragraph" w:styleId="Pre"><w:name w:val="heading 1"/></w:style></w:styles>"#,
        );
        pkg.set("word/numbering.xml", NUMBERING);
        pkg
    }

    fn set(&mut self, name: &str, text: &str) -> &mut Self {
        self.parts.retain(|(n, _)| n != name);
        self.parts.push((name.to_owned(), text.as_bytes().to_vec()));
        self
    }

    fn bytes(&self) -> Bytes {
        let parts: Vec<(&str, &[u8])> = self
            .parts
            .iter()
            .map(|(n, b)| (n.as_str(), b.as_slice()))
            .collect();
        test_zip::build(&parts)
    }

    fn read(&self) -> DocxDocument {
        self.read_with(&ParseLimits::default())
    }

    fn read_with(&self, limits: &ParseLimits) -> DocxDocument {
        read_docx(self.bytes(), limits).expect("document reads")
    }

    fn error(&self, limits: &ParseLimits) -> String {
        match read_docx(self.bytes(), limits) {
            Ok(_) => panic!("expected an error"),
            Err(e) => e.to_string(),
        }
    }
}

fn blocks(body: &str) -> Vec<ParsedBlock> {
    Package::new(body).read().blocks
}

/// A paragraph with properties `ppr` and one run per text.
fn p(ppr: &str, texts: &[&str]) -> String {
    let mut runs = String::new();
    for t in texts {
        write!(runs, r#"<w:r><w:t xml:space="preserve">{t}</w:t></w:r>"#).unwrap();
    }
    format!("<w:p><w:pPr>{ppr}</w:pPr>{runs}</w:p>")
}

fn para(text: &str) -> ParsedBlock {
    ParsedBlock::Paragraph {
        inlines: vec![Inline::plain(text)],
    }
}

fn heading(level: u8, text: &str) -> ParsedBlock {
    ParsedBlock::Heading {
        level,
        inlines: vec![Inline::plain(text)],
    }
}

fn item(level: u8, ordered: bool, text: &str) -> ParsedBlock {
    ParsedBlock::ListItem {
        level,
        ordered,
        blocks: vec![para(text)],
    }
}

fn style(name: &str) -> String {
    format!(r#"<w:pStyle w:val="{name}"/>"#)
}

// --- package ------------------------------------------------------------------------

#[test]
fn main_part_is_found_through_package_relationships() {
    let mut pkg = Package::new("");
    pkg.set(
        "_rels/.rels",
        &rels(&[("rId1", "officeDocument", "doc/main.xml")]),
    );
    pkg.set(
        "doc/main.xml",
        &format!(
            "<w:document {W}><w:body>{}</w:body></w:document>",
            p("", &["moved"])
        ),
    );
    assert_eq!(pkg.read().blocks, [para("moved")]);
}

#[test]
fn styles_come_from_the_styles_relationship_only() {
    // stylesWithEffects names "Pre" a heading; styles.xml makes it code.
    let out = blocks(&p(&style("Pre"), &["x = 1"]));
    assert_eq!(
        out,
        [ParsedBlock::CodeBlock {
            language: None,
            code: "x = 1".to_owned()
        }]
    );
}

#[test]
fn malformed_styles_and_numbering_are_ignored() {
    let mut pkg = Package::new(&p(&style("827"), &["Text"]));
    pkg.set("word/styles.xml", "<w:styles><w:style></w:wrong>");
    pkg.set("word/numbering.xml", "<w:numbering><oops>");
    assert_eq!(pkg.read().blocks, [para("Text")]);
}

#[test]
fn title_comes_from_core_properties() {
    let mut pkg = Package::new("");
    pkg.set(
        "docProps/core.xml",
        r#"<cp:coreProperties xmlns:dc="d"><dc:title>Report</dc:title></cp:coreProperties>"#,
    );
    assert_eq!(pkg.read().title.as_deref(), Some("Report"));
}

#[test]
fn not_a_document_is_an_error() {
    let err = read_docx(Bytes::from_static(b"zip?"), &ParseLimits::default())
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("DOCX"), "{err}");
    let empty = test_zip::build(&[("a.txt", b"x")]);
    let err = read_docx(empty, &ParseLimits::default())
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("no document part"), "{err}");
}

// --- headings, quotes, code ---------------------------------------------------------

#[test]
fn headings_by_outline_name_title_and_id() {
    let body = [
        p(&style("827"), &["One"]),
        p(&style("828"), &["Two"]),
        p(&style("Title"), &["Doc"]),
        p(&style("Subtitle"), &["Sub"]),
        p(&style("Heading3"), &["By id"]),
        p(r#"<w:outlineLvl w:val="1"/>"#, &["Direct"]),
        p(
            &format!("{}<w:outlineLvl w:val=\"9\"/>", style("827")),
            &["Cancelled"],
        ),
    ]
    .concat();
    assert_eq!(
        blocks(&body),
        [
            heading(1, "One"),
            heading(2, "Two"),
            heading(1, "Doc"),
            para("Sub"),
            heading(3, "By id"),
            heading(2, "Direct"),
            para("Cancelled"),
        ]
    );
}

#[test]
fn the_default_paragraph_style_applies() {
    let mut pkg = Package::new(&p("", &["Plain"]));
    pkg.set(
        "word/styles.xml",
        r#"<w:styles xmlns:w="w"><w:style w:type="paragraph" w:default="1" w:styleId="N"><w:name w:val="Normal"/><w:pPr><w:outlineLvl w:val="0"/></w:pPr></w:style></w:styles>"#,
    );
    assert_eq!(pkg.read().blocks, [heading(1, "Plain")]);
}

#[test]
fn style_cycles_end() {
    assert_eq!(blocks(&p(&style("CycleA"), &["x"])), [para("x")]);
}

#[test]
fn quotes_merge_and_follow_base_styles() {
    let body = [
        p(&style("Q"), &["a"]),
        p(&style("MyQuote"), &["b"]),
        p("", &["c"]),
    ]
    .concat();
    assert_eq!(
        blocks(&body),
        [
            ParsedBlock::Quote {
                blocks: vec![para("a"), para("b")]
            },
            para("c")
        ]
    );
}

#[test]
fn code_paragraphs_merge_keeping_blank_lines_and_indents() {
    let body = [
        p(&style("Pre"), &[""]),
        p(&style("Pre"), &["fn main() {"]),
        p(&style("Pre"), &["    run();"]),
        p(&style("Pre"), &[""]),
        p(&style("Pre"), &["}"]),
        p(&style("Pre"), &[""]),
    ]
    .concat();
    assert_eq!(
        blocks(&body),
        [ParsedBlock::CodeBlock {
            language: None,
            code: "fn main() {\n    run();\n\n}".to_owned()
        }]
    );
}

// --- lists --------------------------------------------------------------------------

fn num(id: &str, ilvl: &str) -> String {
    format!(r#"<w:numPr><w:ilvl w:val="{ilvl}"/><w:numId w:val="{id}"/></w:numPr>"#)
}

#[test]
fn list_kinds_follow_the_numbering_formats() {
    let body = [
        p(&num("2", "0"), &["first"]),
        p(&num("2", "1"), &["bullet"]),
        p(&num("2", "2"), &["no marker"]),
        p(&num("3", "0"), &["overridden"]),
        p(&num("0", "0"), &["none"]),
        p(&num("99", "0"), &["unknown"]),
    ]
    .concat();
    assert_eq!(
        blocks(&body),
        [
            item(0, true, "first"),
            item(1, false, "bullet"),
            para("no marker"),
            item(0, false, "overridden"),
            para("none"),
            para("unknown"),
        ]
    );
}

#[test]
fn numbering_from_the_style_and_level_clamping() {
    let body = [
        p(&style("ListBullet"), &["from style"]),
        p(
            &format!("{}{}", style("ListBullet"), num("0", "0")),
            &["cancelled"],
        ),
        p(&num("1", "4294967295"), &["deep"]),
        p(&num("1", "-1"), &["negative"]),
        p(
            &format!("{}{}", style("827"), num("1", "0")),
            &["Heading wins"],
        ),
    ]
    .concat();
    assert_eq!(
        blocks(&body),
        [
            item(0, false, "from style"),
            para("cancelled"),
            item(0, false, "deep"),
            item(0, false, "negative"),
            heading(1, "Heading wins"),
        ]
    );
}

#[test]
fn empty_paragraphs_between_items_are_dropped() {
    let body = [
        p(&num("1", "0"), &["a"]),
        p("", &[]),
        p(&num("1", "0"), &["b"]),
    ]
    .concat();
    assert_eq!(blocks(&body), [item(0, false, "a"), item(0, false, "b")]);
}

// --- runs ---------------------------------------------------------------------------

fn run(rpr: &str, inner: &str) -> String {
    format!("<w:r><w:rPr>{rpr}</w:rPr>{inner}</w:r>")
}

fn t(text: &str) -> String {
    format!(r#"<w:t xml:space="preserve">{text}</w:t>"#)
}

fn styled(text: &str, f: impl FnOnce(&mut InlineStyle)) -> Inline {
    let mut s = InlineStyle::default();
    f(&mut s);
    Inline::styled(text, s)
}

#[test]
fn run_content_kinds() {
    let body = format!(
        "<w:p><w:pPr><w:tabs><w:tab w:val=\"left\" w:pos=\"720\"/></w:tabs></w:pPr>{}{}{}{}{}{}{}</w:p>",
        run("", &format!("{}<w:tab/>{}", t("a"), t("b"))),
        run("", &format!("{}<w:noBreakHyphen/>{}", t(" e"), t("mail"))),
        run("", &format!("{}<w:softHyphen/>{}", t(" co"), t("op"))),
        run(
            "",
            r#"<w:sym w:font="Symbol" w:char="F061"/><w:sym w:font="Wingdings" w:char="F0FC"/><w:sym w:font="Symbol" w:char="zz"/>"#
        ),
        run(
            "",
            &format!(
                "{}<w:cr/>{}<w:br w:type=\"textWrapping\"/>{}",
                t(" x"),
                t("y"),
                t("z")
            )
        ),
        run("", &format!("<w:lastRenderedPageBreak/>{}", t(" same"))),
        "<w:ruby><w:rt><w:r><w:t>kanji</w:t></w:r></w:rt><w:rubyBase><w:r><w:t>\u{6f22}</w:t></w:r></w:rubyBase></w:ruby>",
    );
    assert_eq!(
        blocks(&body),
        [para("a\tb e-mail coop\u{3b1} x\ny\nz same\u{6f22}")]
    );
}

#[test]
fn entities_resolve_and_formatting_whitespace_does_not_leak() {
    let body = "<w:p>\n  <w:r>\n    <w:t>Punctuation &amp; Marks</w:t>\n  </w:r>\n  <w:r><w:t>&#1;!</w:t></w:r>\n</w:p>";
    assert_eq!(blocks(body), [para("Punctuation & Marks!")]);
}

#[test]
fn toggles_underline_and_character_styles() {
    let body = format!(
        "<w:p>{}{}{}{}{}{}</w:p>",
        run("<w:b/>", &t("b")),
        run(r#"<w:b w:val="off"/><w:u w:val="none"/>"#, &t("n")),
        run(r"<w:u/><w:dstrike/>", &t("u")),
        run(r#"<w:rStyle w:val="Strong"/>"#, &t("s")),
        run(r#"<w:rStyle w:val="Src"/>"#, &t("c")),
        run(r#"<w:rFonts w:ascii="Consolas"/><w:caps/>"#, &t("m")),
    );
    assert_eq!(
        blocks(&body),
        [ParsedBlock::Paragraph {
            inlines: vec![
                styled("b", |s| s.bold = true),
                Inline::plain("n"),
                styled("u", |s| {
                    s.underline = true;
                    s.strike = true;
                }),
                styled("s", |s| s.bold = true),
                styled("cm", |s| s.code = true),
            ]
        }]
    );
}

#[test]
fn hidden_text_and_old_properties() {
    let body = format!(
        "<w:p>{}{}{}{}</w:p>",
        run("", &t("a")),
        run("<w:vanish/>", &t("SECRET")),
        run(r#"<w:rStyle w:val="Hide"/>"#, &t("ALSO")),
        run(
            r#"<w:b/><w:rPrChange><w:rPr><w:b w:val="0"/></w:rPr></w:rPrChange>"#,
            &t("b")
        ),
    );
    assert_eq!(
        blocks(&body),
        [ParsedBlock::Paragraph {
            inlines: vec![Inline::plain("a"), styled("b", |s| s.bold = true)]
        }]
    );
    let changed = p(
        &format!(
            "{}<w:pPrChange><w:pPr>{}</w:pPr></w:pPrChange>",
            style("827"),
            style("Normal")
        ),
        &["H"],
    );
    assert_eq!(blocks(&changed), [heading(1, "H")]);
}

#[test]
fn a_hidden_paragraph_mark_keeps_the_text() {
    let body = "<w:p><w:pPr><w:rPr><w:vanish/></w:rPr></w:pPr><w:r><w:t>kept</w:t></w:r></w:p>";
    assert_eq!(blocks(body), [para("kept")]);
}

// --- fields, links, revisions -------------------------------------------------------

fn fld(kind: &str) -> String {
    format!(r#"<w:r><w:fldChar w:fldCharType="{kind}"/></w:r>"#)
}

fn instr(text: &str) -> String {
    format!(r#"<w:r><w:instrText xml:space="preserve">{text}</w:instrText></w:r>"#)
}

fn linked(body: &str) -> Vec<ParsedBlock> {
    Package::with_rels(
        body,
        &[
            ("rH", "hyperlink", "https://example.com/"),
            ("rJ", "hyperlink", "javascript:alert(1)"),
            ("rI", "hyperlink", "local.docx"),
            ("rX", "image", "https://example.com/a.png"),
        ],
    )
    .read()
    .blocks
}

#[test]
fn complex_fields_show_results_only() {
    let body = format!(
        "<w:p>{}{}{}{}{}{}</w:p>",
        fld("begin"),
        instr(r#" HYPERLINK "https://example.com/a" \o "tip" "#),
        fld("separate"),
        format!("{}{}", run("", &t("Exam")), run("", &t("ple"))),
        fld("end"),
        format!(
            "{}{}{}{}{}",
            fld("begin"),
            instr(" PAGE "),
            fld("separate"),
            run("", &t(" 3")),
            fld("end")
        ),
    );
    assert_eq!(
        linked(&body),
        [ParsedBlock::Paragraph {
            inlines: vec![
                Inline::link("Example", "https://example.com/a"),
                Inline::plain(" 3")
            ]
        }]
    );
}

#[test]
fn nested_and_unclosed_fields() {
    let nested = format!(
        "<w:p>{}{}{}{}{}{}{}{}{}</w:p>",
        fld("begin"),
        instr(" IF "),
        fld("begin"),
        instr(" REF x "),
        fld("separate"),
        run("", &t("inner")),
        fld("end"),
        format!("{}{}", fld("separate"), run("", &t("outer"))),
        fld("end"),
    );
    assert_eq!(blocks(&nested), [para("outer")]);
    let unclosed = format!(
        "<w:p>{}{}</w:p>{}{}",
        fld("begin"),
        instr(" TOC "),
        p("", &["next"]),
        p("", &["more"])
    );
    assert_eq!(blocks(&unclosed), [para("next"), para("more")]);
}

#[test]
fn a_table_of_contents_field_spans_paragraphs() {
    let body = format!(
        "<w:p>{}{}{}{}</w:p>{}<w:p>{}{}</w:p>",
        fld("begin"),
        instr(" TOC \\o "),
        fld("separate"),
        run("", &t("Entry 1")),
        p("", &["Entry 2"]),
        run("", &t("Entry 3")),
        fld("end"),
    );
    assert_eq!(
        blocks(&body),
        [para("Entry 1"), para("Entry 2"), para("Entry 3")]
    );
}

#[test]
fn simple_fields() {
    let body = r#"<w:p><w:fldSimple w:instr=" PAGE "><w:r><w:t>3</w:t></w:r></w:fldSimple><w:fldSimple w:instr=' HYPERLINK "https://x.org/" '><w:r><w:t> x</w:t></w:r></w:fldSimple></w:p>"#;
    assert_eq!(
        blocks(body),
        [ParsedBlock::Paragraph {
            inlines: vec![Inline::plain("3"), Inline::link(" x", "https://x.org/")]
        }]
    );
}

#[test]
fn hyperlinks() {
    let body = format!(
        "<w:p>{}{}{}{}{}{}</w:p>",
        format!(
            r#"<w:hyperlink r:id="rH" w:anchor="part">{}</w:hyperlink>"#,
            run("", &t("ext"))
        ),
        format!(
            r#"<w:hyperlink r:id="rJ">{}</w:hyperlink>"#,
            run("", &t(" js"))
        ),
        format!(
            r#"<w:hyperlink w:anchor="_Toc1">{}</w:hyperlink>"#,
            run("", &t(" toc"))
        ),
        format!(
            r#"<w:hyperlink r:id="rX">{}</w:hyperlink>"#,
            run("", &t(" img"))
        ),
        format!(
            r#"<w:hyperlink r:id="rH"><w:hyperlink r:id="rJ">{}</w:hyperlink>{}</w:hyperlink>"#,
            run("", &t(" in")),
            run("", &t(" out"))
        ),
        format!(
            r#"<w:hyperlink r:id="rI">{}</w:hyperlink>"#,
            run("", &t(" rel"))
        ),
    );
    assert_eq!(
        linked(&body),
        [ParsedBlock::Paragraph {
            inlines: vec![
                Inline::link("ext", "https://example.com/#part"),
                Inline::plain(" js toc img in"),
                // The same relationship without an anchor: no anchor.
                Inline::link(" out", "https://example.com/"),
                Inline::plain(" rel"),
            ]
        }]
    );
}

#[test]
fn tracked_changes_read_as_accepted() {
    let body = format!(
        "<w:p>{}<w:ins>{}</w:ins><w:del>{}</w:del><w:moveFrom>{}</w:moveFrom><w:moveTo>{}</w:moveTo></w:p>",
        run("", &t("a")),
        run("", &t("b")),
        r"<w:r><w:delText>c</w:delText></w:r>",
        run("", &t("d")),
        run("", &t("d")),
    );
    assert_eq!(blocks(&body), [para("abd")]);
}

#[test]
fn content_controls_smart_tags_and_placeholders() {
    let body = format!(
        "<w:sdt><w:sdtPr><w:alias w:val=\"x\"/></w:sdtPr><w:sdtContent>{}{}</w:sdtContent></w:sdt>{}<w:p><w:sdt><w:sdtPr><w:showingPlcHdr/></w:sdtPr><w:sdtContent>{}</w:sdtContent></w:sdt><w:smartTag><w:smartTagPr/>{}</w:smartTag></w:p>",
        p("", &["one"]),
        p("", &["two"]),
        "<w:customXml><w:customXmlPr/><w:p><w:r><w:t>three</w:t></w:r></w:p></w:customXml>",
        run("", &t("Click here")),
        run("", &t("tag")),
    );
    assert_eq!(
        blocks(&body),
        [para("one"), para("two"), para("three"), para("tag")]
    );
}

// --- breaks ------------------------------------------------------------------------

#[test]
fn page_breaks_split_paragraphs_and_sections_break_pages() {
    let body = [
        format!(
            "<w:p>{}</w:p>",
            run("", &format!("{}<w:br w:type=\"page\"/>{}", t("a"), t("b")))
        ),
        p(
            r#"<w:sectPr><w:type w:val="continuous"/></w:sectPr>"#,
            &["c"],
        ),
        p("<w:sectPr/>", &["d"]),
        p(&style("Breaker"), &["e"]),
        p("<w:pageBreakBefore/>", &["f"]),
    ]
    .concat();
    assert_eq!(
        blocks(&body),
        [
            para("a"),
            ParsedBlock::PageBreak,
            para("b"),
            para("c"),
            para("d"),
            ParsedBlock::PageBreak,
            para("e"),
            ParsedBlock::PageBreak,
            para("f"),
        ]
    );
}

#[test]
fn no_page_break_at_the_ends() {
    let body = [
        p("<w:pageBreakBefore/>", &["first"]),
        p("<w:sectPr/>", &["last"]),
    ]
    .concat();
    assert_eq!(blocks(&body), [para("first"), para("last")]);
}

#[test]
fn borders_make_rules_only_alone_at_the_bottom() {
    let body = [
        p(r#"<w:pBdr><w:bottom w:val="single"/></w:pBdr>"#, &[]),
        p(r#"<w:pBdr><w:top w:val="single"/><w:left w:val="single"/><w:bottom w:val="single"/></w:pBdr>"#, &[]),
        p(r#"<w:pBdr><w:bottom w:val="single"/></w:pBdr>"#, &["text"]),
        "<w:p><w:r><w:pict><v:rect xmlns:v=\"v\" xmlns:o=\"o\" o:hr=\"t\"/></w:pict></w:r></w:p>".to_owned(),
    ]
    .concat();
    assert_eq!(
        blocks(&body),
        [
            ParsedBlock::HorizontalRule,
            para("text"),
            ParsedBlock::HorizontalRule
        ]
    );
}

// --- drawings ------------------------------------------------------------------------

fn drawing(docpr: &str) -> String {
    format!(
        "<w:r><w:drawing><wp:inline>{docpr}<a:graphic xmlns:a=\"a\"/></wp:inline></w:drawing></w:r>"
    )
}

#[test]
fn pictures_with_descriptions_become_images() {
    let body = format!(
        "<w:p>{}{}{}{}{}{}</w:p>",
        run("", &t("before")),
        drawing(
            r#"<wp:docPr id="1" name="Picture 1" descr="A chart"><a:hlinkClick xmlns:a="a"/></wp:docPr>"#
        ),
        drawing(r#"<wp:docPr id="2" name="Picture 2"/>"#),
        drawing(r#"<wp:docPr id="3" descr="hidden" hidden="1"/>"#),
        drawing(
            r#"<wp:docPr id="4" descr="Ornament"><a:extLst xmlns:a="a"><a:ext><adec:decorative xmlns:adec="x" val="1"/></a:ext></a:extLst></wp:docPr>"#
        ),
        run("", &t("after")),
    );
    assert_eq!(
        blocks(&body),
        [
            para("before"),
            ParsedBlock::Image {
                alt: Some("A chart".to_owned()),
                title: None,
                src: None
            },
            para("after"),
        ]
    );
}

#[test]
fn alternate_content_reads_one_branch() {
    let body = format!(
        "<w:p><w:r><mc:AlternateContent><mc:Choice Requires=\"wps\">{}</mc:Choice><mc:Fallback>{}</mc:Fallback></mc:AlternateContent></w:r></w:p>",
        r#"<w:drawing><wp:inline><wp:docPr id="1" descr="once"/></wp:inline></w:drawing>"#,
        r#"<w:pict><v:shape xmlns:v="v"><v:textbox><w:txbxContent><w:p><w:r><w:t>FALLBACK</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:pict>"#,
    );
    assert_eq!(
        blocks(&body),
        [ParsedBlock::Image {
            alt: Some("once".to_owned()),
            title: None,
            src: None
        }]
    );
}

#[test]
fn text_boxes_follow_their_paragraph() {
    let body = format!(
        "<w:p>{}<w:r><w:drawing><wps:txbx xmlns:wps=\"s\"><w:txbxContent>{}</w:txbxContent></wps:txbx></w:drawing></w:r>{}</w:p>{}",
        run("", &t("before ")),
        p("", &["boxed"]),
        run("", &t("after")),
        p("", &["next"]),
    );
    assert_eq!(
        blocks(&body),
        [para("before after"), para("boxed"), para("next")]
    );
}

#[test]
fn note_and_comment_references_leave_no_text() {
    let body = format!(
        "<w:p>{}<w:r><w:footnoteReference w:id=\"1\"/></w:r><w:r><w:commentReference w:id=\"0\"/></w:r>{}</w:p>",
        run("", &t("sentence")),
        run("", &t(" ends.")),
    );
    assert_eq!(blocks(&body), [para("sentence ends.")]);
}

// --- tables ---------------------------------------------------------------------------

fn tc(tcpr: &str, text: &str) -> String {
    format!("<w:tc><w:tcPr>{tcpr}</w:tcPr>{}</w:tc>", p("", &[text]))
}

fn tbl(rows: &str) -> String {
    format!(r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="1"/></w:tblGrid>{rows}</w:tbl>"#)
}

fn cell(text: &str) -> TableCell {
    TableCell {
        blocks: vec![para(text)],
    }
}

fn row(is_header: bool, cells: &[&str]) -> TableRow {
    TableRow {
        is_header,
        cells: cells.iter().map(|c| cell(c)).collect(),
    }
}

#[test]
fn tables_with_spans_merges_and_ragged_rows() {
    let rows = [
        format!("<w:tr>{}{}</w:tr>", tc("", "H1"), tc("", "H2")),
        format!(
            "<w:tr>{}{}</w:tr>",
            tc(
                r#"<w:gridSpan w:val="2"/><w:vMerge w:val="restart"/>"#,
                "wide"
            ),
            tc("", "x")
        ),
        format!(
            "<w:tr><w:trPr><w:gridBefore w:val=\"1\"/></w:trPr>{}</w:tr>",
            tc("<w:vMerge/>", "GONE")
        ),
        "<w:tr><w:tc><w:p/></w:tc></w:tr>".to_owned(),
    ]
    .concat();
    assert_eq!(
        blocks(&tbl(&rows)),
        [ParsedBlock::Table(TableBlock {
            rows: vec![
                row(true, &["H1", "H2", ""]),
                row(false, &["wide", "", "x"]),
                row(false, &["", "", ""]),
                row(false, &["", "", ""]),
            ]
        })]
    );
}

#[test]
fn repeating_header_rows_are_headers() {
    let rows = [
        format!(
            "<w:tr><w:trPr><w:tblHeader/></w:trPr>{}</w:tr>",
            tc("", "a")
        ),
        format!(
            "<w:tr><w:trPr><w:tblHeader w:val=\"true\"/></w:trPr>{}</w:tr>",
            tc("", "b")
        ),
        format!("<w:tr>{}</w:tr>", tc("", "c")),
    ]
    .concat();
    let out = blocks(&tbl(&rows));
    let ParsedBlock::Table(table) = &out[0] else {
        panic!("{out:?}");
    };
    let headers: Vec<bool> = table.rows.iter().map(|r| r.is_header).collect();
    assert_eq!(headers, [true, true, false]);
}

#[test]
fn huge_spans_are_clamped_and_charged() {
    let rows = format!(
        "<w:tr>{}</w:tr>",
        tc(r#"<w:gridSpan w:val="1000000"/>"#, "x")
    );
    let out = blocks(&tbl(&rows));
    let ParsedBlock::Table(table) = &out[0] else {
        panic!("{out:?}");
    };
    assert_eq!(table.rows[0].cells.len(), 1000);

    let many = format!("<w:tr>{}</w:tr>", tc(r#"<w:gridSpan w:val="1000"/>"#, "x")).repeat(200);
    let out = blocks(&tbl(&many));
    assert_eq!(out, [crate::infra::parsers::limits::skipped_table_note(0)]);
}

#[test]
fn nested_tables_are_read_into_the_cell() {
    let inner = tbl(&format!(
        "<w:tr>{}{}</w:tr><w:tr>{}{}</w:tr>",
        tc("", "a"),
        tc("", "b"),
        tc("", "c"),
        tc("", "d")
    ));
    let rows = format!("<w:tr><w:tc>{inner}</w:tc>{}</w:tr>", tc("", "side"));
    let out = blocks(&tbl(&rows));
    let ParsedBlock::Table(table) = &out[0] else {
        panic!("{out:?}");
    };
    assert_eq!(
        table.rows[0].cells[0].blocks,
        [para("a"), para("b"), para("c"), para("d")]
    );
}

#[test]
fn headings_and_breaks_in_cells_stay_paragraph_text() {
    let content = format!(
        "{}<w:p><w:r>{}<w:br w:type=\"page\"/>{}</w:r></w:p>",
        p(&style("827"), &["Cell heading"]),
        t("x"),
        t("y")
    );
    let rows = format!("<w:tr><w:tc>{content}</w:tc></w:tr>");
    let out = blocks(&tbl(&rows));
    let ParsedBlock::Table(table) = &out[0] else {
        panic!("{out:?}");
    };
    assert_eq!(
        table.rows[0].cells[0].blocks,
        [para("Cell heading"), para("x\ny")]
    );
}

#[test]
fn rtl_text_is_kept_as_stored() {
    let hebrew = "\u{5e9}\u{5dc}\u{5d5}\u{5dd}";
    assert_eq!(blocks(&p("<w:bidi/>", &[hebrew])), [para(hebrew)]);
}

// --- limits ----------------------------------------------------------------------------

#[test]
fn deep_nesting_hits_the_depth_limit() {
    let depth = 3000;
    let body = format!(
        "{}{}{}",
        "<w:sdt><w:sdtContent>".repeat(depth),
        p("", &["deep"]),
        "</w:sdtContent></w:sdt>".repeat(depth)
    );
    let err = Package::new(&body).error(&ParseLimits::default());
    assert!(err.contains("max_nesting_depth"), "{err}");
}

#[test]
fn text_limit_is_enforced() {
    let limits = ParseLimits {
        max_text_bytes: 10,
        ..ParseLimits::default()
    };
    let err = Package::new(&p("", &["more than ten bytes"])).error(&limits);
    assert!(err.contains("max_text_bytes"), "{err}");
}

#[test]
fn deeply_nested_tables_do_not_overflow() {
    let depth = 300;
    let mut body = String::new();
    for _ in 0..depth {
        body.push_str("<w:tbl><w:tr><w:tc>");
    }
    body.push_str(&p("", &["core"]));
    for _ in 0..depth {
        body.push_str("</w:tc></w:tr></w:tbl>");
    }
    // A table inside a cell joins that cell, past MAX_TABLE_DEPTH too: one
    // table with one cell is left.
    assert_eq!(
        blocks(&body),
        [ParsedBlock::Table(TableBlock {
            rows: vec![row(true, &["core"])]
        })]
    );
}

// --- budgets, fields and edge cases ----------------------------------------------------

#[test]
fn empty_code_paragraphs_with_page_breaks_stay_linear() {
    let mut pkg = Package::new(&p(&style("CodeBreak"), &[]).repeat(20_000));
    pkg.set(
        "word/styles.xml",
        r#"<w:styles xmlns:w="w"><w:style w:type="paragraph" w:styleId="CodeBreak"><w:name w:val="Code"/><w:pPr><w:pageBreakBefore/></w:pPr></w:style></w:styles>"#,
    );
    let limits = ParseLimits {
        max_compression_ratio: 100_000,
        ..ParseLimits::default()
    };
    assert!(pkg.read_with(&limits).blocks.is_empty());
    // What the body gives for a million of them, tidied directly: a few
    // tenths of a second. Removing page breaks one at a time takes about
    // ten minutes, far past nextest's two-minute kill.
    let empty_code = ParsedBlock::CodeBlock {
        language: None,
        code: String::new(),
    };
    let body: Vec<ParsedBlock> = [empty_code, ParsedBlock::PageBreak]
        .into_iter()
        .cycle()
        .take(1_000_000)
        .collect();
    assert!(tidy(body).is_empty());
}

#[test]
fn long_style_names_are_read_once() {
    // An 8 MB style name and 40,000 paragraphs: reading the name once is
    // quick; reading it again per paragraph moves 3e11 bytes, several
    // minutes, far past nextest's two-minute kill.
    let name = "x".repeat(8_000_000);
    let mut pkg = Package::new(&p("", &["a"]).repeat(40_000));
    pkg.set(
        "word/styles.xml",
        &format!(r#"<w:styles xmlns:w="w"><w:style w:type="paragraph" w:default="1" w:styleId="N"><w:name w:val="{name}"/></w:style></w:styles>"#),
    );
    let limits = ParseLimits {
        max_compression_ratio: 100_000,
        ..ParseLimits::default()
    };
    assert_eq!(pkg.read_with(&limits).blocks.len(), 40_000);
}

#[test]
fn link_targets_count_against_the_text_budget() {
    let long = format!("https://example.com/{}", "a".repeat(1_500));
    let runs = format!("{}{}", run("<w:b/>", &t("x")), run("", &t("y"))).repeat(50);
    let body = format!(r#"<w:p><w:hyperlink r:id="rL">{runs}</w:hyperlink></w:p>"#);
    let pkg = Package::with_rels(&body, &[("rL", "hyperlink", &long)]);
    let limits = ParseLimits {
        max_text_bytes: 20_000,
        ..ParseLimits::default()
    };
    let err = pkg.error(&limits);
    assert!(err.contains("max_text_bytes"), "{err}");
}

#[test]
fn nested_tables_share_the_cell_budget() {
    let limits = ParseLimits {
        max_table_cells: 50,
        ..ParseLimits::default()
    };
    let filler = tc(r#"<w:gridSpan w:val="30"/>"#, "f");
    let mut body = String::new();
    for _ in 0..5 {
        write!(body, "<w:tbl><w:tr>{filler}<w:tc>").unwrap();
    }
    body.push_str(&p("", &["core"]));
    for _ in 0..5 {
        body.push_str("</w:tc></w:tr></w:tbl>");
    }
    let out = Package::new(&body).read_with(&limits).blocks;
    assert_eq!(
        out,
        [crate::infra::parsers::limits::skipped_table_note(0)],
        "{out:?}"
    );
}

#[test]
fn skipped_table_notes_are_charged() {
    // The first table spends the one-cell budget; each later one becomes a
    // note, which must cost text like any paragraph.
    let empty = "<w:tbl><w:tr><w:tc></w:tc><w:tc></w:tc></w:tr></w:tbl>".repeat(200);
    let limits = ParseLimits {
        max_table_cells: 1,
        max_text_bytes: 10_000,
        ..ParseLimits::default()
    };
    let err = Package::new(&empty).error(&limits);
    assert!(err.contains("max_text_bytes"), "{err}");
}

#[test]
fn rules_and_images_are_charged_as_blocks() {
    let rule =
        r#"<w:p><w:r><w:pict><v:rect xmlns:v="v" xmlns:o="o" o:hr="t"/></w:pict></w:r></w:p>"#;
    let image = r#"<w:p><w:r><w:drawing><wp:inline><wp:docPr id="1" descr="a"/></wp:inline></w:drawing></w:r></w:p>"#;
    let body = format!("{rule}{image}").repeat(100);
    let with = |max_text_bytes| ParseLimits {
        max_text_bytes,
        ..ParseLimits::default()
    };
    // 200 blocks and the images' one-byte alt texts.
    let cost = 200 * BLOCK_COST + 100;
    assert_eq!(Package::new(&body).read_with(&with(cost)).blocks.len(), 200);
    let err = Package::new(&body).error(&with(cost - 1));
    assert!(err.contains("max_text_bytes"), "{err}");
}

#[test]
fn simple_field_instructions_are_cut_like_complex_ones() {
    // Only the first 2048 characters are read, so a huge instruction is
    // never split into tokens whole; a target past them is not seen.
    let pad = " ".repeat(2_100);
    let body = format!(
        r#"<w:p><w:fldSimple w:instr='HYPERLINK{pad}"https://x.org/"'>{}</w:fldSimple></w:p>"#,
        run("", &t("x"))
    );
    assert_eq!(blocks(&body), [para("x")]);
}

#[test]
fn anchors_belong_to_each_hyperlink() {
    let body = format!(
        r#"<w:p><w:hyperlink r:id="rH" w:anchor="intro">{}</w:hyperlink><w:hyperlink r:id="rH" w:anchor="usage">{}</w:hyperlink></w:p>"#,
        run("", &t("a")),
        run("", &t("b"))
    );
    assert_eq!(
        linked(&body),
        [ParsedBlock::Paragraph {
            inlines: vec![
                Inline::link("a", "https://example.com/#intro"),
                Inline::link("b", "https://example.com/#usage"),
            ]
        }]
    );
}

#[test]
fn hidden_styles_boxes_and_pictures_stay_hidden() {
    let mut pkg = Package::new(&format!(
        "{}<w:p>{}<w:r><w:rPr><w:vanish/></w:rPr><w:drawing><wps:txbx xmlns:wps=\"s\"><w:txbxContent>{}</w:txbxContent></wps:txbx></w:drawing></w:r><w:r><w:rPr><w:vanish/></w:rPr>{}</w:r></w:p>",
        p(&style("Secret"), &["SECRET"]),
        run("", &t("shown")),
        p("", &["BOXED"]),
        r#"<w:drawing><wp:inline><wp:docPr id="1" descr="HIDDEN ALT"/></wp:inline></w:drawing>"#,
    ));
    pkg.set(
        "word/styles.xml",
        r#"<w:styles xmlns:w="w"><w:style w:type="paragraph" w:styleId="Secret"><w:name w:val="Secret"/><w:rPr><w:vanish/></w:rPr></w:style></w:styles>"#,
    );
    assert_eq!(pkg.read().blocks, [para("shown")]);
}

#[test]
fn list_styles_link_numbering() {
    let mut pkg = Package::new(&p(&num("5", "0"), &["linked item"]));
    pkg.set(
        "word/numbering.xml",
        r#"<w:numbering xmlns:w="w">
<w:abstractNum w:abstractNumId="1"><w:numStyleLink w:val="MyList"/></w:abstractNum>
<w:abstractNum w:abstractNumId="2"><w:styleLink w:val="MyList"/><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum>
<w:num w:numId="5"><w:abstractNumId w:val="1"/></w:num>
<w:num w:numId="6"><w:abstractNumId w:val="2"/></w:num>
</w:numbering>"#,
    );
    pkg.set(
        "word/styles.xml",
        r#"<w:styles xmlns:w="w"><w:style w:type="numbering" w:styleId="MyList"><w:name w:val="My List"/><w:pPr><w:numPr><w:numId w:val="6"/></w:numPr></w:pPr></w:style></w:styles>"#,
    );
    assert_eq!(pkg.read().blocks, [item(0, true, "linked item")]);
}

#[test]
fn heading_breaks_and_edge_tabs() {
    let title = format!(
        "<w:p><w:pPr>{}</w:pPr>{}</w:p>",
        style("Title"),
        run("", &format!("{}<w:br/>{}", t("Annual Report"), t("2024")))
    );
    let tabbed = format!(
        "<w:p>{}</w:p><w:p>{}</w:p>",
        run("", &format!("<w:tab/>{}", t("Indented"))),
        run("", "<w:tab/><w:tab/>")
    );
    assert_eq!(
        blocks(&format!("{title}{tabbed}")),
        [heading(1, "Annual Report 2024"), para("Indented")]
    );
}

#[test]
fn a_placeholder_does_not_hide_the_next_content_control() {
    let body = format!(
        "<w:sdt><w:sdtPr><w:showingPlcHdr/></w:sdtPr><w:sdtContent/></w:sdt><w:sdt><w:sdtPr/><w:sdtContent>{}</w:sdtContent></w:sdt>",
        p("", &["REAL CONTENT"])
    );
    assert_eq!(blocks(&body), [para("REAL CONTENT")]);
}

#[test]
fn an_unclosed_field_in_a_simple_field_does_not_link_the_rest() {
    let body = format!(
        r#"<w:p><w:fldSimple w:instr=' HYPERLINK "https://x.org/" '>{}{}{}</w:fldSimple></w:p>{}"#,
        run("", &t("link")),
        fld("begin"),
        instr(" PAGE "),
        p("", &["after"])
    );
    assert_eq!(
        blocks(&body),
        [
            ParsedBlock::Paragraph {
                inlines: vec![Inline::link("link", "https://x.org/")]
            },
            para("after")
        ]
    );
}

#[test]
fn an_empty_section_break_breaks_the_page() {
    let body = [p("<w:sectPr/>", &["one"]), p("", &["two"])].concat();
    assert_eq!(
        blocks(&body),
        [para("one"), ParsedBlock::PageBreak, para("two")]
    );
}

#[test]
fn deleted_rows_and_cells_are_dropped() {
    let rows = [
        format!(
            "<w:tr>{}{}</w:tr>",
            tc("", "keep"),
            tc("<w:cellDel/>", "gone")
        ),
        format!(
            "<w:tr><w:trPr><w:del w:id=\"1\"/></w:trPr>{}</w:tr>",
            tc("", "deleted row")
        ),
        format!("<w:tr>{}</w:tr>", tc("", "last")),
    ]
    .concat();
    assert_eq!(
        blocks(&tbl(&rows)),
        [ParsedBlock::Table(TableBlock {
            rows: vec![row(true, &["keep"]), row(false, &["last"])]
        })]
    );
}

#[test]
fn style_and_font_names_are_matched_exactly() {
    let mut pkg = Package::new(&format!(
        "{}<w:p>{}</w:p>",
        p(&style("Zip"), &["12345"]),
        run(r#"<w:rFonts w:ascii="Monotype Corsiva"/>"#, &t("fancy"))
    ));
    pkg.set(
        "word/styles.xml",
        r#"<w:styles xmlns:w="w"><w:style w:type="paragraph" w:styleId="Zip"><w:name w:val="Zip Code Field"/></w:style></w:styles>"#,
    );
    assert_eq!(pkg.read().blocks, [para("12345"), para("fancy")]);
}

#[test]
fn symbols_and_legacy_picture_alt_text() {
    let body = format!(
        "<w:p>{}<w:r><w:pict><v:shape xmlns:v=\"v\" alt=\"Logo\"/></w:pict></w:r></w:p>",
        run("", r#"<w:sym w:font="Symbol" w:char="F040"/>"#)
    );
    assert_eq!(
        blocks(&body),
        [
            para("\u{2245}"),
            ParsedBlock::Image {
                alt: Some("Logo".to_owned()),
                title: None,
                src: None
            }
        ]
    );
}

#[test]
fn localized_heading_names_without_outline_levels_are_body_text() {
    let mut pkg = Package::new(&p(&style("Z1"), &["Title?"]));
    pkg.set(
        "word/styles.xml",
        "<w:styles xmlns:w=\"w\"><w:style w:type=\"paragraph\" w:styleId=\"Z1\"><w:name w:val=\"\u{417}\u{430}\u{433}\u{43e}\u{43b}\u{43e}\u{432}\u{43e}\u{43a} 1\"/></w:style></w:styles>",
    );
    assert_eq!(pkg.read().blocks, [para("Title?")]);
}

#[test]
fn nested_text_boxes_are_read() {
    let depth = 20;
    let mut inner = p("", &["innermost"]);
    for _ in 0..depth {
        inner = format!(
            "<w:p><w:r><w:drawing><w:txbxContent>{inner}</w:txbxContent></w:drawing></w:r></w:p>"
        );
    }
    // Boxes nest apart to MAX_BOX_DEPTH, deeper ones join their host: the
    // text comes out once.
    assert_eq!(blocks(&inner), [para("innermost")]);
}

#[test]
fn fields_nested_past_the_cap_stay_balanced() {
    let depth = MAX_FIELDS + 5;
    let mut body = String::from("<w:p>");
    for _ in 0..depth {
        body.push_str(&fld("begin"));
        body.push_str(&instr(" IF "));
    }
    for level in (0..depth).rev() {
        body.push_str(&fld("separate"));
        body.push_str(&run("", &t(&format!("r{level}"))));
        body.push_str(&fld("end"));
    }
    body.push_str("</w:p>");
    body.push_str(&p("", &["after"]));
    // Inner results are part of the outer instructions; only the outermost
    // result shows, and the next paragraph is untouched.
    assert_eq!(blocks(&body), [para("r0"), para("after")]);
}

#[test]
fn truncated_and_mangled_documents_never_panic() {
    let body = format!(
        "{}{}<w:p>{}{}{}{}{}</w:p>{}<w:sdt><w:sdtContent>{}</w:sdtContent></w:sdt>",
        p(&style("Heading1"), &["Title"]),
        tbl(&format!(
            "<w:tr>{}{}</w:tr>",
            tc(r#"<w:gridSpan w:val="2"/>"#, "cell"),
            tc("", &tbl(&format!("<w:tr>{}</w:tr>", tc("", "inner"))))
        )),
        fld("begin"),
        instr(r#" HYPERLINK "https://example.com/" "#),
        fld("separate"),
        run("<w:b/>", &t("link")),
        fld("end"),
        "<w:p><w:r><w:drawing><w:txbxContent><w:p><w:r><w:t>box</w:t></w:r></w:p></w:txbxContent></w:drawing></w:r></w:p>",
        p("", &["in sdt"]),
    );
    let document = format!("<w:document {W}><w:body>{body}<w:sectPr/></w:body></w:document>");
    let mut seed: u32 = 0x2545_F491;
    for cut in (0..document.len()).step_by(5) {
        let Some(prefix) = document.get(..cut) else {
            continue;
        };
        let mut pkg = Package::new("");
        pkg.set("word/document.xml", prefix);
        drop(read_docx(pkg.bytes(), &ParseLimits::default()));
        // Swap two random bytes of the ASCII document.
        let mut bytes = document.clone().into_bytes();
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let len = u32::try_from(bytes.len()).unwrap();
        let i = usize::try_from(seed % len).unwrap();
        let j = usize::try_from((seed >> 7) % len).unwrap();
        bytes.swap(i, j);
        pkg.set("word/document.xml", &String::from_utf8_lossy(&bytes));
        drop(read_docx(pkg.bytes(), &ParseLimits::default()));
    }
}

// --- revisions, legacy pictures, styles and merges ---------------------------------------

fn styles_with(extra: &str) -> String {
    STYLES.replace("</w:styles>", &format!("{extra}</w:styles>"))
}

#[test]
fn legacy_vml_pictures_come_out_once_per_shape() {
    let pict = |inner: &str| format!("<w:p><w:r><w:pict>{inner}</w:pict></w:r></w:p>");
    let body = [
        pict(r#"<v:shape xmlns:v="v" xmlns:o="o" alt="Chart"><v:imagedata r:id="rI" o:title="chart1"/></v:shape>"#),
        pict(&format!(
            r#"<v:shape xmlns:v="v" alt="Box"><v:textbox><w:txbxContent>{}</w:txbxContent></v:textbox></v:shape>"#,
            p("", &["boxed"])
        )),
        pict(r#"<v:rect xmlns:v="v" xmlns:o="o"><v:imagedata o:title="framed"/></v:rect>"#),
    ]
    .concat();
    assert_eq!(
        blocks(&body),
        [
            ParsedBlock::Image {
                alt: Some("Chart".to_owned()),
                title: Some("chart1".to_owned()),
                src: None
            },
            para("boxed"),
            ParsedBlock::Image {
                alt: None,
                title: Some("framed".to_owned()),
                src: None
            },
        ]
    );
}

#[test]
fn a_deleted_paragraph_mark_joins_the_next_paragraph() {
    let deleted = r#"<w:rPr><w:del w:id="1" w:author="a"/></w:rPr>"#;
    let body = [
        // The joined paragraph takes the properties of the mark that stays.
        p(deleted, &["one "]),
        p(&style("827"), &["two"]),
        // All of it deleted: nothing is left.
        format!(
            "<w:p><w:pPr>{deleted}</w:pPr><w:del>{}</w:del></w:p>",
            r"<w:r><w:delText>gone</w:delText></w:r>"
        ),
        p("", &["three"]),
        // A table ends the joined paragraph.
        p(deleted, &["four"]),
        tbl(&format!("<w:tr>{}</w:tr>", tc("", "cell"))),
        // A mark with other run properties stays.
        p(r"<w:rPr><w:b/><w:ins w:id='2'/></w:rPr>", &["five"]),
        p("", &["six"]),
    ]
    .concat();
    assert_eq!(
        blocks(&body),
        [
            heading(1, "one two"),
            para("three"),
            para("four"),
            ParsedBlock::Table(TableBlock {
                rows: vec![row(true, &["cell"])]
            }),
            para("five"),
            para("six"),
        ]
    );
}

#[test]
fn a_picture_in_a_text_box_keeps_its_own_shape() {
    let body = format!(
        "<w:p><w:r><w:pict>{}</w:pict></w:r></w:p>",
        r#"<v:shape xmlns:v="v" xmlns:o="o"><v:textbox><w:txbxContent><w:p><w:r><w:pict><v:rect><v:imagedata o:title="inner"/></v:rect></w:pict></w:r></w:p></w:txbxContent></v:textbox></v:shape>"#
    );
    assert_eq!(
        blocks(&body),
        [ParsedBlock::Image {
            alt: None,
            title: Some("inner".to_owned()),
            src: None
        }]
    );
}

#[test]
fn a_moved_away_paragraph_mark_joins_the_next_paragraph() {
    let body = [
        p(
            r#"<w:rPr><w:moveFrom w:id="1" w:author="a"/></w:rPr>"#,
            &["one "],
        ),
        p("", &["two"]),
    ]
    .concat();
    assert_eq!(blocks(&body), [para("one two")]);
}

#[test]
fn the_nearest_style_decides_the_role() {
    let mut pkg = Package::new(
        &[
            p(&style("QuoteOn9"), &["quoted"]),
            p("", &["-"]),
            p(&style("TitleOn9"), &["titled"]),
            p(&style("BodyOnH1"), &["body"]),
            p(
                &format!("{}<w:outlineLvl w:val=\"9\"/>", style("Q")),
                &["still quoted"],
            ),
            p("", &["-"]),
            p(
                &format!("{}<w:outlineLvl w:val=\"9\"/>", style("Pre")),
                &["still code"],
            ),
            p(
                &format!("{}<w:outlineLvl w:val=\"9\"/>", style("Heading3")),
                &["not by id"],
            ),
        ]
        .concat(),
    );
    pkg.set(
        "word/styles.xml",
        &styles_with(concat!(
            r#"<w:style w:type="paragraph" w:styleId="Body9"><w:name w:val="Body9"/><w:pPr><w:outlineLvl w:val="9"/></w:pPr></w:style>"#,
            r#"<w:style w:type="paragraph" w:styleId="QuoteOn9"><w:name w:val="Quote"/><w:basedOn w:val="Body9"/></w:style>"#,
            r#"<w:style w:type="paragraph" w:styleId="TitleOn9"><w:name w:val="Title"/><w:basedOn w:val="Body9"/></w:style>"#,
            r#"<w:style w:type="paragraph" w:styleId="BodyOnH1"><w:name w:val="Mine"/><w:basedOn w:val="827"/><w:pPr><w:outlineLvl w:val="9"/></w:pPr></w:style>"#,
        )),
    );
    let quote = |text| ParsedBlock::Quote {
        blocks: vec![para(text)],
    };
    assert_eq!(
        pkg.read().blocks,
        [
            quote("quoted"),
            para("-"),
            heading(1, "titled"),
            para("body"),
            quote("still quoted"),
            para("-"),
            ParsedBlock::CodeBlock {
                language: None,
                code: "still code".to_owned()
            },
            para("not by id"),
        ]
    );
}

#[test]
fn hyperlink_fields_with_unsafe_targets_stay_text() {
    let complex = |instruction: &str, text: &str| {
        format!(
            "{}{}{}{}{}",
            fld("begin"),
            instr(instruction),
            fld("separate"),
            run("", &t(text)),
            fld("end")
        )
    };
    let body = format!(
        "<w:p>{}{}{}{}{}{}</w:p>",
        complex(r#" HYPERLINK "javascript:alert(1)" "#, "a"),
        complex(r#" HYPERLINK "data:text/html,x" "#, " b"),
        r#"<w:fldSimple w:instr=' HYPERLINK "javascript:alert(1)" '><w:r><w:t> c</w:t></w:r></w:fldSimple>"#,
        r#"<w:fldSimple w:instr=' HYPERLINK "data:text/html,x" '><w:r><w:t> d</w:t></w:r></w:fldSimple>"#,
        complex(r#" HYPERLINK "https://example.com/" \l "usage" "#, "e"),
        complex(r#" HYPERLINK \l "_Toc1" "#, " f"),
    );
    assert_eq!(
        blocks(&body),
        [ParsedBlock::Paragraph {
            inlines: vec![
                Inline::plain("a b c d"),
                Inline::link("e", "https://example.com/#usage"),
                Inline::plain(" f"),
            ]
        }]
    );
}

#[test]
fn horizontal_merges_and_grid_after() {
    let rows = [
        format!(
            "<w:tr><w:trPr><w:gridAfter w:val=\"2\"/></w:trPr>{}{}</w:tr>",
            tc(r#"<w:hMerge w:val="restart"/>"#, "merged"),
            tc("<w:hMerge/>", "GONE")
        ),
        format!(
            "<w:tr>{}{}{}{}</w:tr>",
            tc("", "a"),
            tc("", "b"),
            tc("", "c"),
            tc("", "d")
        ),
    ]
    .concat();
    assert_eq!(
        blocks(&tbl(&rows)),
        [ParsedBlock::Table(TableBlock {
            rows: vec![
                row(true, &["merged", "", "", ""]),
                row(false, &["a", "b", "c", "d"]),
            ]
        })]
    );
}

#[test]
fn spans_are_clamped_to_the_column_limit() {
    let limits = ParseLimits {
        max_table_columns: 5,
        ..ParseLimits::default()
    };
    let rows = format!(
        "<w:tr><w:trPr><w:gridBefore w:val=\"100\"/></w:trPr></w:tr><w:tr>{}</w:tr>",
        tc(r#"<w:gridSpan w:val="100"/>"#, "x")
    );
    let out = Package::new(&tbl(&rows)).read_with(&limits).blocks;
    let ParsedBlock::Table(table) = &out[0] else {
        panic!("{out:?}");
    };
    let widths: Vec<usize> = table.rows.iter().map(|r| r.cells.len()).collect();
    assert_eq!(widths, [5, 5]);
    assert_eq!(table.rows[1].cells[0], cell("x"));
}

#[test]
fn picture_titles_are_kept() {
    let body = format!(
        "<w:p>{}</w:p>",
        drawing(r#"<wp:docPr id="1" name="Picture 1" descr=" " title="Logo"/>"#)
    );
    assert_eq!(
        blocks(&body),
        [ParsedBlock::Image {
            alt: None,
            title: Some("Logo".to_owned()),
            src: None
        }]
    );
}

#[test]
fn text_boxes_in_table_cells_stay_in_the_cell() {
    let content = format!(
        "<w:p>{}<w:r><w:drawing><wps:txbx xmlns:wps=\"s\"><w:txbxContent>{}</w:txbxContent></wps:txbx></w:drawing></w:r></w:p>",
        run("", &t("host")),
        p("", &["boxed"])
    );
    let rows = format!("<w:tr><w:tc>{content}</w:tc>{}</w:tr>", tc("", "side"));
    let out = blocks(&tbl(&rows));
    assert_eq!(
        out,
        [ParsedBlock::Table(TableBlock {
            rows: vec![TableRow {
                is_header: true,
                cells: vec![
                    TableCell {
                        blocks: vec![para("host"), para("boxed")]
                    },
                    cell("side"),
                ]
            }]
        })]
    );
}
