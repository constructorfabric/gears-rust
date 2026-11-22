use std::fmt::Write as _;

use super::styles::{Bullet, ListStyle, Placeholder};
use super::*;

use crate::domain::ir::{InlineStyle, TableBlock, TableCell, TableRow};
use crate::infra::parsers::ooxml::test_zip;

const NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006""#;

const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// A relationships part; targets with a scheme are external.
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

fn slide_xml(shapes: &str) -> String {
    format!(r"<p:sld {NS}><p:cSld><p:spTree><p:nvGrpSpPr/>{shapes}</p:spTree></p:cSld></p:sld>")
}

/// A shape with placeholder element `ph` (may be empty) and paragraphs.
fn sp(ph: &str, paras: &str) -> String {
    format!(
        r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="s"/><p:cNvSpPr/><p:nvPr>{ph}</p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/>{paras}</p:txBody></p:sp>"#
    )
}

fn title_sp(text: &str) -> String {
    sp(r#"<p:ph type="title"/>"#, &para(text))
}

fn body_sp(paras: &str) -> String {
    sp(r#"<p:ph idx="1"/>"#, paras)
}

fn text_box(paras: &str) -> String {
    sp("", paras)
}

fn para(text: &str) -> String {
    format!("<a:p><a:r><a:t>{text}</a:t></a:r></a:p>")
}

const MASTER: &str = r#"<p:sldMaster xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree>
<p:sp><p:nvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:txBody><a:lstStyle/><a:p><a:r><a:t>Click to edit Master title style</a:t></a:r></a:p></p:txBody></p:sp>
<p:sp><p:nvSpPr><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:txBody><a:lstStyle/><a:p><a:r><a:t>MASTER BODY PROMPT</a:t></a:r></a:p></p:txBody></p:sp>
<p:sp><p:nvSpPr><p:nvPr><p:ph type="dt" idx="2"/></p:nvPr></p:nvSpPr><p:txBody><a:lstStyle><a:lvl1pPr><a:buNone/></a:lvl1pPr></a:lstStyle><a:p/></p:txBody></p:sp>
<p:sp><p:nvSpPr><p:nvPr/></p:nvSpPr><p:txBody><a:p><a:r><a:t>MASTER LOGO TEXT</a:t></a:r></a:p></p:txBody></p:sp>
</p:spTree></p:cSld><p:txStyles>
<p:titleStyle><a:lvl1pPr><a:buNone/></a:lvl1pPr></p:titleStyle>
<p:bodyStyle><a:lvl1pPr marL="342900"><a:buFont typeface="Arial"/><a:buChar char="&#8226;"/></a:lvl1pPr><a:lvl2pPr><a:buChar char="-"/></a:lvl2pPr><a:lvl3pPr><a:buChar char="-"/></a:lvl3pPr><a:lvl9pPr><a:buChar char="-"/></a:lvl9pPr></p:bodyStyle>
<p:otherStyle><a:lvl1pPr/></p:otherStyle>
</p:txStyles></p:sldMaster>"#;

const LAYOUT: &str = r#"<p:sldLayout xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree>
<p:sp><p:nvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:txBody><a:lstStyle/><a:p><a:r><a:t>Click to edit</a:t></a:r></a:p></p:txBody></p:sp>
<p:sp><p:nvSpPr><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr><p:txBody><a:lstStyle/><a:p><a:r><a:t>LAYOUT PROMPT</a:t></a:r></a:p></p:txBody></p:sp>
<p:sp><p:nvSpPr><p:nvPr><p:ph type="subTitle" idx="3"/></p:nvPr></p:nvSpPr><p:txBody><a:lstStyle><a:lvl1pPr><a:buNone/></a:lvl1pPr></a:lstStyle></p:txBody></p:sp>
<p:sp><p:nvSpPr><p:nvPr><p:ph idx="4"/></p:nvPr></p:nvSpPr><p:txBody><a:lstStyle><a:lvl1pPr><a:buNone/></a:lvl1pPr></a:lstStyle></p:txBody></p:sp>
<p:sp><p:nvSpPr><p:nvPr><p:ph type="ftr" idx="11"/></p:nvPr></p:nvSpPr></p:sp>
<p:sp><p:nvSpPr><p:nvPr/></p:nvSpPr><p:txBody><a:p><a:r><a:t>LAYOUT ONLY</a:t></a:r></a:p></p:txBody></p:sp>
</p:spTree></p:cSld></p:sldLayout>"#;

/// A package under construction: the standard parts plus overrides.
struct Package {
    parts: Vec<(String, Vec<u8>)>,
}

impl Package {
    /// A deck of `slides` (slide XML), each on the standard layout.
    fn new(slides: &[String]) -> Self {
        let mut ids = String::new();
        let mut pres_rels = vec![(
            "rIdM".to_owned(),
            "slideMaster",
            "slideMasters/slideMaster1.xml".to_owned(),
        )];
        let mut pkg = Self { parts: Vec::new() };
        for (i, slide) in slides.iter().enumerate() {
            let n = i + 1;
            write!(ids, r#"<p:sldId id="{}" r:id="rId{n}"/>"#, 255 + n).unwrap();
            pres_rels.push((format!("rId{n}"), "slide", format!("slides/slide{n}.xml")));
            pkg.set(&format!("ppt/slides/slide{n}.xml"), slide);
            pkg.set(
                &format!("ppt/slides/_rels/slide{n}.xml.rels"),
                &rels(&[("rIdL", "slideLayout", "../slideLayouts/slideLayout1.xml")]),
            );
        }
        let pres_rels: Vec<(&str, &str, &str)> = pres_rels
            .iter()
            .map(|(id, kind, target)| (id.as_str(), *kind, target.as_str()))
            .collect();
        pkg.set(
            "_rels/.rels",
            &rels(&[
                ("rId1", "officeDocument", "ppt/presentation.xml"),
                ("rId2", "metadata/core-properties", "docProps/core.xml"),
            ]),
        );
        pkg.set(
            "ppt/presentation.xml",
            &format!(r"<p:presentation {NS}><p:sldIdLst>{ids}</p:sldIdLst></p:presentation>"),
        );
        pkg.set("ppt/_rels/presentation.xml.rels", &rels(&pres_rels));
        pkg.set("ppt/slideLayouts/slideLayout1.xml", LAYOUT);
        pkg.set(
            "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
            &rels(&[("rId1", "slideMaster", "../slideMasters/slideMaster1.xml")]),
        );
        pkg.set("ppt/slideMasters/slideMaster1.xml", MASTER);
        pkg.set(
            "ppt/slideMasters/_rels/slideMaster1.xml.rels",
            &rels(&[("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml")]),
        );
        pkg
    }

    fn set(&mut self, name: &str, text: &str) -> &mut Self {
        self.parts.retain(|(n, _)| n != name);
        self.parts.push((name.to_owned(), text.as_bytes().to_vec()));
        self
    }

    fn remove(&mut self, name: &str) -> &mut Self {
        self.parts.retain(|(n, _)| n != name);
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

    fn read(&self) -> Deck {
        self.read_with(&ParseLimits::default())
    }

    fn read_with(&self, limits: &ParseLimits) -> Deck {
        read_deck(self.bytes(), limits).expect("deck reads")
    }

    fn error(&self, limits: &ParseLimits) -> String {
        match read_deck(self.bytes(), limits) {
            Ok(_) => panic!("expected an error"),
            Err(e) => e.to_string(),
        }
    }
}

fn one_slide(shapes: &str) -> Vec<ParsedBlock> {
    Package::new(&[slide_xml(shapes)]).read().blocks
}

fn h2(text: &str) -> ParsedBlock {
    ParsedBlock::Heading {
        level: 2,
        inlines: vec![Inline::plain(text)],
    }
}

fn p(text: &str) -> ParsedBlock {
    ParsedBlock::Paragraph {
        inlines: vec![Inline::plain(text)],
    }
}

fn li(level: u8, ordered: bool, text: &str) -> ParsedBlock {
    ParsedBlock::ListItem {
        level,
        ordered,
        blocks: vec![p(text)],
    }
}

fn headings(blocks: &[ParsedBlock]) -> Vec<String> {
    blocks
        .iter()
        .filter_map(|b| match b {
            ParsedBlock::Heading { inlines, .. } => Some(
                inlines
                    .iter()
                    .map(|i| match i {
                        Inline::Text { text, .. }
                        | Inline::Link { text, .. }
                        | Inline::Code { text, .. } => text.as_str(),
                    })
                    .collect(),
            ),
            _ => None,
        })
        .collect()
}

fn all_text(blocks: &[ParsedBlock]) -> String {
    format!("{blocks:?}")
}

// --- slides and order ---------------------------------------------------

#[test]
fn slides_follow_the_slide_list_not_file_names() {
    let mut pkg = Package::new(&[slide_xml(&title_sp("B")), slide_xml(&title_sp("A"))]);
    pkg.set(
        "ppt/presentation.xml",
        &format!(
            r#"<p:presentation {NS}><p:sldIdLst><p:sldId id="300" r:id="rId2"/><p:sldId id="301" r:id="rId1"/></p:sldIdLst></p:presentation>"#
        ),
    );
    assert_eq!(headings(&pkg.read().blocks), ["A", "B"]);
}

#[test]
fn page_breaks_go_between_slides_only() {
    let deck = Package::new(&[slide_xml(""), slide_xml(""), slide_xml("")]).read();
    assert_eq!(
        deck.blocks,
        [
            h2("Slide 1"),
            ParsedBlock::PageBreak,
            h2("Slide 2"),
            ParsedBlock::PageBreak,
            h2("Slide 3"),
        ]
    );
    assert!(Package::new(&[]).read().blocks.is_empty());
}

#[test]
fn duplicate_and_dangling_slide_ids_are_read_once() {
    let mut pkg = Package::new(&[slide_xml(&title_sp("Only"))]);
    pkg.set(
        "ppt/presentation.xml",
        &format!(
            r#"<p:presentation {NS}><p:sldIdLst><p:sldId r:id="rId1"/><p:sldId r:id="rId1"/><p:sldId r:id="rId404"/><p:sldId r:id="rIdM"/></p:sldIdLst></p:presentation>"#
        ),
    );
    assert_eq!(pkg.read().blocks, [h2("Only")]);
}

#[test]
fn missing_slide_list_falls_back_to_slide_relationships() {
    let mut pkg = Package::new(&[slide_xml(&title_sp("One")), slide_xml(&title_sp("Two"))]);
    pkg.set("ppt/presentation.xml", &format!("<p:presentation {NS}/>"));
    assert_eq!(headings(&pkg.read().blocks), ["One", "Two"]);
}

#[test]
fn a_missing_slide_part_keeps_the_numbering() {
    let mut pkg = Package::new(&[slide_xml(""), slide_xml(""), slide_xml("")]);
    pkg.remove("ppt/slides/slide2.xml");
    assert_eq!(headings(&pkg.read().blocks), ["Slide 1", "Slide 3"]);
}

#[test]
fn hidden_slides_are_kept() {
    let hidden = format!(
        r#"<p:sld {NS} show="0"><p:cSld><p:spTree>{}</p:spTree></p:cSld></p:sld>"#,
        title_sp("Backup")
    );
    let deck = Package::new(&[slide_xml(&title_sp("Main")), hidden]).read();
    assert_eq!(headings(&deck.blocks), ["Main", "Backup"]);
}

#[test]
fn slide_without_shape_tree_is_an_empty_slide() {
    let deck = Package::new(&[format!(r"<p:sld {NS}><p:clrMapOvr/></p:sld>")]).read();
    assert_eq!(deck.blocks, [h2("Slide 1")]);
}

#[test]
fn main_part_is_found_through_package_relationships() {
    let mut pkg = Package::new(&[]);
    let pres = pkg
        .parts
        .iter()
        .find(|(n, _)| n == "ppt/presentation.xml")
        .map(|(_, b)| String::from_utf8(b.clone()).unwrap())
        .unwrap();
    pkg.remove("ppt/presentation.xml");
    pkg.remove("ppt/_rels/presentation.xml.rels");
    pkg.set(
        "deck/main.xml",
        &pres.replace(
            "<p:sldIdLst></p:sldIdLst>",
            r#"<p:sldIdLst><p:sldId r:id="s1"/></p:sldIdLst>"#,
        ),
    );
    pkg.set(
        "deck/_rels/main.xml.rels",
        &rels(&[("s1", "slide", "s/one.xml")]),
    );
    pkg.set("deck/s/one.xml", &slide_xml(&title_sp("Moved")));
    pkg.set(
        "_rels/.rels",
        &rels(&[("rId1", "officeDocument", "deck/main.xml")]),
    );
    assert_eq!(pkg.read().blocks, [h2("Moved")]);
}

#[test]
fn not_a_presentation_is_an_error() {
    let err = read_deck(Bytes::from_static(b"not a zip"), &ParseLimits::default())
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("PPTX"), "{err}");
    let empty_zip = test_zip::build(&[("a.txt", b"x")]);
    let err = read_deck(empty_zip, &ParseLimits::default())
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("no presentation part"), "{err}");
}

// --- titles ---------------------------------------------------------------

#[test]
fn title_placeholder_becomes_the_heading_once() {
    let blocks = one_slide(&format!("{}{}", title_sp("Agenda"), text_box(&para("Hi"))));
    assert_eq!(blocks, [h2("Agenda"), p("Hi")]);
}

#[test]
fn title_text_joins_runs_verbatim_and_breaks_as_spaces() {
    let title = sp(
        r#"<p:ph type="ctrTitle"/>"#,
        r#"<a:p><a:r><a:rPr b="1"/><a:t>Pow</a:t></a:r><a:r><a:t>erPoint</a:t></a:r><a:br/><a:r><a:t>Line  2</a:t></a:r></a:p><a:p><a:r><a:t>Line3</a:t></a:r></a:p>"#,
    );
    assert_eq!(one_slide(&title), [h2("PowerPoint Line 2 Line3")]);
}

#[test]
fn no_title_falls_back_to_slide_number_without_guessing() {
    let blocks = one_slide(&text_box(&para("Short")));
    assert_eq!(blocks, [h2("Slide 1"), p("Short")]);
}

#[test]
fn empty_title_lets_the_next_title_win_and_extra_titles_are_paragraphs() {
    let shapes = format!(
        "{}{}{}",
        sp(r#"<p:ph type="title"/>"#, "<a:p/>"),
        title_sp("Real"),
        title_sp("Second")
    );
    assert_eq!(one_slide(&shapes), [h2("Real"), p("Second")]);
}

#[test]
fn title_inside_a_group_is_found() {
    let shapes = format!(
        "<p:grpSp><p:nvGrpSpPr/><p:grpSpPr/><p:grpSp>{}</p:grpSp>{}</p:grpSp>",
        title_sp("Grouped"),
        text_box(&para("After"))
    );
    assert_eq!(one_slide(&shapes), [h2("Grouped"), p("After")]);
}

#[test]
fn document_title_comes_from_core_properties() {
    let mut pkg = Package::new(&[]);
    pkg.set(
        "docProps/core.xml",
        r#"<cp:coreProperties xmlns:cp="c" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title> Quarterly </dc:title></cp:coreProperties>"#,
    );
    assert_eq!(pkg.read().title.as_deref(), Some("Quarterly"));
    pkg.set(
        "docProps/core.xml",
        r#"<cp:coreProperties xmlns:dc="d"><dc:title>  </dc:title></cp:coreProperties>"#,
    );
    assert_eq!(pkg.read().title, None);
    pkg.set(
        "docProps/core.xml",
        "<cp:coreProperties><dc:title>broken</dc:other></cp:coreProperties>",
    );
    assert_eq!(pkg.read().title, None, "malformed metadata is ignored");
}

// --- bullets ----------------------------------------------------------------

#[test]
fn body_placeholder_inherits_bullets_from_the_master() {
    let body = body_sp(&format!(
        "{}{}",
        para("One"),
        r#"<a:p><a:pPr lvl="1"/><a:r><a:t>Nested</a:t></a:r></a:p>"#
    ));
    assert_eq!(
        one_slide(&body),
        [h2("Slide 1"), li(0, false, "One"), li(1, false, "Nested")]
    );
}

#[test]
fn text_boxes_and_subtitles_are_not_bulleted() {
    let shapes = format!(
        "{}{}",
        text_box(&para("Box")),
        sp(r#"<p:ph type="subTitle" idx="3"/>"#, &para("Sub"))
    );
    assert_eq!(one_slide(&shapes), [h2("Slide 1"), p("Box"), p("Sub")]);
}

#[test]
fn layout_placeholder_is_matched_by_index() {
    // idx 4 turns bullets off on the layout; idx 1 inherits the master's.
    let shapes = format!(
        "{}{}",
        sp(r#"<p:ph idx="4"/>"#, &para("Plain")),
        body_sp(&para("Bullet"))
    );
    assert_eq!(
        one_slide(&shapes),
        [h2("Slide 1"), p("Plain"), li(0, false, "Bullet")]
    );
}

#[test]
fn master_placeholder_is_matched_by_kind_not_index() {
    // The master's idx 2 is the date placeholder (no bullets); a slide body
    // placeholder with idx 2 and no layout counterpart is still body text.
    let shapes = sp(r#"<p:ph idx="2"/>"#, &para("Content"));
    assert_eq!(one_slide(&shapes), [h2("Slide 1"), li(0, false, "Content")]);
}

#[test]
fn bullet_choice_on_the_paragraph_wins() {
    let paras = concat!(
        r"<a:p><a:pPr><a:buNone/></a:pPr><a:r><a:t>Off</a:t></a:r></a:p>",
        r#"<a:p><a:pPr><a:buAutoNum type="arabicPeriod"/></a:pPr><a:r><a:t>Num</a:t></a:r></a:p>"#,
        r#"<a:p><a:pPr><a:buFont typeface="X"/></a:pPr><a:r><a:t>Inherits</a:t></a:r></a:p>"#,
    );
    assert_eq!(
        one_slide(&body_sp(paras)),
        [
            h2("Slide 1"),
            p("Off"),
            li(0, true, "Num"),
            li(0, false, "Inherits")
        ]
    );
    let boxed = concat!(
        r"<a:p><a:pPr><a:buBlip><a:blip/></a:buBlip></a:pPr><a:r><a:t>Pic</a:t></a:r></a:p>",
        r#"<a:p><a:pPr lvl="1"/><a:r><a:t>Level only</a:t></a:r></a:p>"#,
    );
    assert_eq!(
        one_slide(&text_box(boxed)),
        [h2("Slide 1"), li(0, false, "Pic"), p("Level only")]
    );
}

#[test]
fn shape_list_style_overrides_the_layout() {
    let shape = r#"<p:sp><p:nvSpPr><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr><p:txBody><a:lstStyle><a:lvl1pPr><a:buAutoNum type="romanUcPeriod"/></a:lvl1pPr></a:lstStyle><a:p><a:r><a:t>I</a:t></a:r></a:p></p:txBody></p:sp>"#;
    assert_eq!(one_slide(shape), [h2("Slide 1"), li(0, true, "I")]);
}

#[test]
fn list_levels_are_clamped() {
    let paras = concat!(
        r#"<a:p><a:pPr lvl="4294967294"/><a:r><a:t>Deep</a:t></a:r></a:p>"#,
        r#"<a:p><a:pPr lvl="-1"/><a:r><a:t>Neg</a:t></a:r></a:p>"#,
        r#"<a:p><a:pPr lvl="x"/><a:r><a:t>Junk</a:t></a:r></a:p>"#,
    );
    assert_eq!(
        one_slide(&body_sp(paras)),
        [
            h2("Slide 1"),
            li(8, false, "Deep"),
            li(0, false, "Neg"),
            li(0, false, "Junk")
        ]
    );
}

#[test]
fn empty_paragraphs_are_dropped() {
    let paras = format!(
        "{}<a:p><a:endParaRPr/></a:p><a:p><a:r><a:t>  </a:t></a:r></a:p>{}",
        para("A"),
        para("B")
    );
    assert_eq!(
        one_slide(&body_sp(&paras)),
        [h2("Slide 1"), li(0, false, "A"), li(0, false, "B")]
    );
}

#[test]
fn slide_without_layout_uses_only_its_own_bullets() {
    let mut pkg = Package::new(&[slide_xml(&body_sp(&para("Plain")))]);
    pkg.remove("ppt/slides/_rels/slide1.xml.rels");
    assert_eq!(pkg.read().blocks, [h2("Slide 1"), p("Plain")]);
}

#[test]
fn layouts_and_masters_add_no_text() {
    let text = all_text(&one_slide(&format!("{}{}", title_sp("T"), body_sp(""))));
    for prompt in ["Click to edit", "LAYOUT", "MASTER"] {
        assert!(!text.contains(prompt), "{prompt} leaked: {text}");
    }
}

#[test]
fn shared_layout_is_read_once() {
    // Each read of the layout would cost about 40 KB of the archive budget.
    let padding = format!("<!--{}-->", "x".repeat(40_000));
    let slides: Vec<String> = (0..20).map(|_| slide_xml(&body_sp(&para("x")))).collect();
    let mut pkg = Package::new(&slides);
    pkg.set(
        "ppt/slideLayouts/slideLayout1.xml",
        &LAYOUT.replace("<p:cSld>", &format!("{padding}<p:cSld>")),
    );
    let limits = ParseLimits {
        max_total_uncompressed_bytes: 200_000,
        ..ParseLimits::default()
    };
    assert_eq!(headings(&pkg.read_with(&limits).blocks).len(), 20);
}

// --- runs and text ----------------------------------------------------------

#[test]
fn fields_breaks_tabs_and_split_words() {
    let paras = concat!(
        r#"<a:p><a:r><a:t>Page </a:t></a:r><a:fld id="{1}" type="slidenum"><a:t>3</a:t></a:fld>"#,
        r"<a:br/><a:r><a:t>Hel</a:t></a:r><a:r><a:t>lo&#9;there&#11;x</a:t></a:r></a:p>",
    );
    assert_eq!(
        one_slide(&text_box(paras)),
        [h2("Slide 1"), p("Page 3\nHello\tthere\nx")]
    );
}

#[test]
fn run_styles_follow_attribute_values() {
    let paras = concat!(
        r#"<a:p><a:r><a:rPr b="1" i="true" u="dbl" strike="sngStrike"/><a:t>all</a:t></a:r>"#,
        r#"<a:r><a:rPr b="0" i="false" u="none" strike="noStrike"/><a:t> none</a:t></a:r></a:p>"#,
    );
    let all = InlineStyle {
        bold: true,
        italic: true,
        underline: true,
        strike: true,
        code: false,
    };
    assert_eq!(
        one_slide(&text_box(paras)),
        [
            h2("Slide 1"),
            ParsedBlock::Paragraph {
                inlines: vec![Inline::styled("all", all), Inline::plain(" none")]
            }
        ]
    );
}

#[test]
fn markup_like_text_stays_text() {
    let blocks = one_slide(&text_box(&para("# Fake | a | ![x](y)")));
    assert_eq!(blocks, [h2("Slide 1"), p("# Fake | a | ![x](y)")]);
}

// --- links ------------------------------------------------------------------

fn linked_slide(runs: &str, slide_rels: &[(&str, &str, &str)]) -> Vec<ParsedBlock> {
    let mut pkg = Package::new(&[slide_xml(&text_box(&format!("<a:p>{runs}</a:p>")))]);
    let mut all = vec![("rIdL", "slideLayout", "../slideLayouts/slideLayout1.xml")];
    all.extend_from_slice(slide_rels);
    pkg.set("ppt/slides/_rels/slide1.xml.rels", &rels(&all));
    pkg.read().blocks
}

fn link_run(text: &str, attrs: &str) -> String {
    format!(r"<a:r><a:rPr><a:hlinkClick {attrs}/></a:rPr><a:t>{text}</a:t></a:r>")
}

#[test]
fn external_links_are_kept_and_adjacent_runs_merge() {
    let runs = format!(
        "{}{}",
        link_run("Exa", r#"r:id="rIdH""#),
        link_run("mple", r#"r:id="rIdH""#)
    );
    let blocks = linked_slide(&runs, &[("rIdH", "hyperlink", "https://example.com/")]);
    assert_eq!(
        blocks,
        [
            h2("Slide 1"),
            ParsedBlock::Paragraph {
                inlines: vec![Inline::link("Example", "https://example.com/")]
            }
        ]
    );
}

#[test]
fn actions_internal_targets_and_unsafe_schemes_are_plain_text() {
    let runs = format!(
        "{}{}{}{}",
        link_run("jump", r#"r:id="rIdS" action="ppaction://hlinksldjump""#),
        link_run(
            " next",
            r#"r:id="" action="ppaction://hlinkshowjump?jump=nextslide""#
        ),
        link_run(" js", r#"r:id="rIdJ""#),
        link_run(" file", r#"r:id="rIdF" action="ppaction://hlinkfile""#),
    );
    let blocks = linked_slide(
        &runs,
        &[
            ("rIdS", "slide", "slide2.xml"),
            ("rIdJ", "hyperlink", "javascript:alert(1)"),
            ("rIdF", "hyperlink", "file:///etc/passwd"),
        ],
    );
    assert_eq!(blocks, [h2("Slide 1"), p("jump next js file")]);
}

// --- alternate content, hidden shapes, furniture ------------------------------

#[test]
fn alternate_content_reads_one_branch() {
    let shapes = format!(
        "<mc:AlternateContent><mc:Choice Requires=\"p14\">{}</mc:Choice><mc:Fallback>{}</mc:Fallback></mc:AlternateContent>",
        text_box(&para("Choice")),
        text_box(&para("Fallback"))
    );
    assert_eq!(one_slide(&shapes), [h2("Slide 1"), p("Choice")]);
}

#[test]
fn equation_text_is_kept_once() {
    let paras = concat!(
        r#"<a:p><a:r><a:t>x = </a:t></a:r><mc:AlternateContent><mc:Choice Requires="a14">"#,
        r"<a14:m><m:oMath><m:r><m:t>y</m:t></m:r><m:sSup><m:e><m:r><m:t>2</m:t></m:r></m:e></m:sSup></m:oMath></a14:m>",
        r"</mc:Choice><mc:Fallback><a:r><a:t>[image]</a:t></a:r></mc:Fallback></mc:AlternateContent></a:p>",
    );
    assert_eq!(one_slide(&text_box(paras)), [h2("Slide 1"), p("x = y2")]);
}

#[test]
fn hidden_shapes_and_groups_are_skipped() {
    let hidden_shape = r#"<p:sp><p:nvSpPr><p:cNvPr id="5" name="h" hidden="1"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:txBody><a:p><a:r><a:t>SECRET</a:t></a:r></a:p></p:txBody></p:sp>"#;
    let hidden_group = format!(
        r#"<p:grpSp><p:nvGrpSpPr><p:cNvPr id="6" name="g" hidden="true"></p:cNvPr><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>{}</p:grpSp>"#,
        text_box(&para("ALSO SECRET"))
    );
    let shapes = format!("{hidden_shape}{hidden_group}{}", text_box(&para("Visible")));
    assert_eq!(one_slide(&shapes), [h2("Slide 1"), p("Visible")]);
}

#[test]
fn footer_is_kept_and_date_and_number_are_dropped() {
    let shapes = format!(
        "{}{}{}",
        sp(r#"<p:ph type="ftr" idx="11"/>"#, &para("Confidential")),
        sp(
            r#"<p:ph type="sldNum" idx="12"/>"#,
            r#"<a:p><a:fld type="slidenum"><a:t>7</a:t></a:fld></a:p>"#
        ),
        sp(r#"<p:ph type="dt" idx="10"/>"#, &para("1/2/2026")),
    );
    assert_eq!(one_slide(&shapes), [h2("Slide 1"), p("Confidential")]);
}

#[test]
fn speaker_notes_are_not_read() {
    let mut pkg = Package::new(&[slide_xml(&title_sp("T"))]);
    pkg.set(
        "ppt/slides/_rels/slide1.xml.rels",
        &rels(&[
            ("rIdL", "slideLayout", "../slideLayouts/slideLayout1.xml"),
            ("rIdN", "notesSlide", "../notesSlides/notesSlide1.xml"),
        ]),
    );
    pkg.set(
        "ppt/notesSlides/notesSlide1.xml",
        &format!(
            r"<p:notes {NS}><p:cSld><p:spTree>{}</p:spTree></p:cSld></p:notes>",
            body_sp(&para("NOTE-TEXT"))
        ),
    );
    assert!(!all_text(&pkg.read().blocks).contains("NOTE-TEXT"));
}

// --- tables -------------------------------------------------------------------

fn table_frame(rows: &str) -> String {
    format!(
        r#"<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="3" name="t"/></p:nvGraphicFramePr><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table"><a:tbl><a:tblPr firstRow="1"/><a:tblGrid/>{rows}</a:tbl></a:graphicData></a:graphic></p:graphicFrame>"#
    )
}

fn tc(text: &str) -> String {
    format!(
        "<a:tc><a:txBody><a:bodyPr/>{}</a:txBody><a:tcPr/></a:tc>",
        para(text)
    )
}

fn cell(text: &str) -> TableCell {
    TableCell {
        blocks: if text.is_empty() {
            Vec::new()
        } else {
            vec![p(text)]
        },
    }
}

fn row(is_header: bool, cells: &[&str]) -> TableRow {
    TableRow {
        is_header,
        cells: cells.iter().map(|c| cell(c)).collect(),
    }
}

#[test]
fn table_rows_are_padded_and_merged_cells_stay_empty() {
    let rows = format!(
        "<a:tr>{}{}</a:tr><a:tr>{}<a:tc hMerge=\"1\"><a:txBody><a:p/></a:txBody></a:tc>{}</a:tr><a:tr>{}</a:tr><a:tr/>",
        tc("H1"),
        tc("H2"),
        r#"<a:tc gridSpan="4294967295"><a:txBody><a:p><a:r><a:t>Wide</a:t></a:r></a:p></a:txBody></a:tc>"#,
        tc("C"),
        tc("Short"),
    );
    let blocks = one_slide(&table_frame(&rows));
    assert_eq!(
        blocks,
        [
            h2("Slide 1"),
            ParsedBlock::Table(TableBlock {
                rows: vec![
                    row(true, &["H1", "H2", ""]),
                    row(false, &["Wide", "", "C"]),
                    row(false, &["Short", "", ""]),
                    row(false, &["", "", ""]),
                ]
            })
        ]
    );
}

#[test]
fn table_without_cells_emits_nothing() {
    assert_eq!(one_slide(&table_frame("<a:tr/>")), [h2("Slide 1")]);
}

#[test]
fn table_cells_keep_only_their_own_bullets() {
    let rows = format!(
        r#"<a:tr>{}<a:tc><a:txBody><a:p><a:pPr><a:buChar char="-"/></a:pPr><a:r><a:t>B</a:t></a:r></a:p></a:txBody></a:tc></a:tr>"#,
        tc("A")
    );
    let blocks = one_slide(&table_frame(&rows));
    let ParsedBlock::Table(table) = &blocks[1] else {
        panic!("expected a table: {blocks:?}");
    };
    assert_eq!(table.rows[0].cells[1].blocks, [li(0, false, "B")]);
}

#[test]
fn tables_over_the_cell_budget_become_notes_and_spend_it() {
    let big = format!("<a:tr>{}{}</a:tr>", tc("a"), tc("b")).repeat(3);
    let small = format!("<a:tr>{}</a:tr>", tc("ok"));
    let limits = ParseLimits {
        max_table_cells: 5,
        ..ParseLimits::default()
    };
    let shapes = format!("{}{}", table_frame(&big), table_frame(&small));
    let blocks = Package::new(&[slide_xml(&shapes)])
        .read_with(&limits)
        .blocks;
    // The skipped table's 6 cells used up the budget, so the next one is
    // skipped too.
    assert_eq!(
        blocks,
        [
            h2("Slide 1"),
            crate::infra::parsers::limits::skipped_table_note(0),
            crate::infra::parsers::limits::skipped_table_note(0),
        ]
    );
}

#[test]
fn empty_rows_count_against_the_cell_budget() {
    let limits = ParseLimits {
        max_table_cells: 3,
        ..ParseLimits::default()
    };
    let rows = format!("{}<a:tr>{}</a:tr>", "<a:tr/>".repeat(10), tc("x"));
    let blocks = Package::new(&[slide_xml(&table_frame(&rows))])
        .read_with(&limits)
        .blocks;
    assert!(
        matches!(&blocks[1], ParsedBlock::Paragraph { .. }),
        "expected a skip note: {blocks:?}"
    );
}

// --- pictures and SmartArt ----------------------------------------------------

fn pic(cnvpr: &str) -> String {
    format!(
        r#"<p:pic><p:nvPicPr>{cnvpr}<p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId9"/></p:blipFill><p:spPr/></p:pic>"#
    )
}

#[test]
fn pictures_keep_their_description() {
    let shapes = format!(
        "{}{}{}{}",
        pic(r#"<p:cNvPr id="4" name="p" descr=" A chart " title="Logo"/>"#),
        pic(r#"<p:cNvPr id="5" name="p"/>"#),
        pic(
            r#"<p:cNvPr id="6" name="p" descr="Ornament"><a:extLst><a:ext uri="{C183D7F6}"><adec:decorative xmlns:adec="x" val="1"/></a:ext></a:extLst></p:cNvPr>"#
        ),
        pic(r#"<p:cNvPr id="7" name="p" descr="" title="Only title"/>"#),
    );
    assert_eq!(
        one_slide(&shapes),
        [
            h2("Slide 1"),
            ParsedBlock::Image {
                alt: Some("A chart".to_owned()),
                title: Some("Logo".to_owned()),
                src: None
            },
            ParsedBlock::Image {
                alt: None,
                title: Some("Only title".to_owned()),
                src: None
            },
        ]
    );
}

fn diagram_frame(rel: &str) -> String {
    format!(
        r#"<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="8" name="d"/></p:nvGraphicFramePr><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/diagram"><dgm:relIds xmlns:dgm="d" r:dm="{rel}" r:lo="rIdLo"/></a:graphicData></a:graphic></p:graphicFrame>"#
    )
}

#[test]
fn smartart_node_text_is_placed_where_the_frame_is() {
    let shapes = format!(
        "{}{}{}",
        text_box(&para("Before")),
        diagram_frame("rIdD"),
        text_box(&para("After"))
    );
    let mut pkg = Package::new(&[slide_xml(&shapes)]);
    pkg.set(
        "ppt/slides/_rels/slide1.xml.rels",
        &rels(&[
            ("rIdL", "slideLayout", "../slideLayouts/slideLayout1.xml"),
            ("rIdD", "diagramData", "../diagrams/data1.xml"),
        ]),
    );
    pkg.set(
        "ppt/diagrams/data1.xml",
        r#"<dgm:dataModel xmlns:dgm="d" xmlns:a="a"><dgm:ptLst>
<dgm:pt modelId="0" type="doc"><dgm:t><a:p><a:r><a:t>DOC</a:t></a:r></a:p></dgm:t></dgm:pt>
<dgm:pt modelId="1"><dgm:t><a:bodyPr/><a:p><a:r><a:t>Plan</a:t></a:r></a:p></dgm:t></dgm:pt>
<dgm:pt modelId="2" type="parTrans"><dgm:t><a:p><a:r><a:t>TRANS</a:t></a:r></a:p></dgm:t></dgm:pt>
<dgm:pt modelId="3" type="asst"><dgm:t><a:p><a:r><a:t>Do</a:t></a:r></a:p><a:p/></dgm:t></dgm:pt>
<dgm:pt modelId="4" type="pres"><dgm:prSet phldrT="[Text]"/></dgm:pt>
</dgm:ptLst></dgm:dataModel>"#,
    );
    assert_eq!(
        pkg.read().blocks,
        [h2("Slide 1"), p("Before"), p("Plan"), p("Do"), p("After")]
    );
}

#[test]
fn smartart_with_a_dangling_or_escaping_target_adds_nothing() {
    let mut pkg = Package::new(&[slide_xml(&format!(
        "{}{}",
        diagram_frame("rIdX"),
        diagram_frame("rIdE")
    ))]);
    pkg.set(
        "ppt/slides/_rels/slide1.xml.rels",
        &rels(&[
            ("rIdL", "slideLayout", "../slideLayouts/slideLayout1.xml"),
            ("rIdE", "diagramData", "../../../../etc/passwd"),
        ]),
    );
    assert_eq!(pkg.read().blocks, [h2("Slide 1")]);
}

// --- hostile input --------------------------------------------------------------

#[test]
fn deep_group_nesting_hits_the_depth_limit() {
    let depth = 5_000;
    let shapes = format!(
        "{}{}{}",
        "<p:grpSp>".repeat(depth),
        text_box(&para("deep")),
        "</p:grpSp>".repeat(depth)
    );
    let err = Package::new(&[slide_xml(&shapes)]).error(&ParseLimits::default());
    assert!(err.contains("max_nesting_depth"), "{err}");
}

#[test]
fn text_limit_is_enforced() {
    let limits = ParseLimits {
        max_text_bytes: 10,
        ..ParseLimits::default()
    };
    let err = Package::new(&[slide_xml(&text_box(&para("more than ten bytes")))]).error(&limits);
    assert!(err.contains("max_text_bytes"), "{err}");
}

#[test]
fn misplaced_structures_are_skipped() {
    // A table inside a shape and a shape inside a shape are not valid
    // PresentationML; their content is skipped, the rest is read.
    let shapes = format!(
        "<p:sp><p:nvSpPr/>{}<a:tbl><a:tr>{}</a:tr></a:tbl></p:sp>{}",
        text_box(&para("inner")),
        tc("cell"),
        text_box(&para("ok"))
    );
    assert_eq!(one_slide(&shapes), [h2("Slide 1"), p("ok")]);
}

#[test]
fn truncated_and_mangled_slides_never_panic() {
    let slide = slide_xml(&format!(
        "{}{}{}{}",
        title_sp("T"),
        body_sp(
            r#"<a:p><a:pPr lvl="1"><a:buAutoNum/></a:pPr><a:r><a:rPr b="1"><a:hlinkClick r:id="x"/></a:rPr><a:t>a</a:t></a:r></a:p>"#
        ),
        table_frame(&format!("<a:tr>{}</a:tr>", tc("c"))),
        pic(r#"<p:cNvPr id="1" descr="d"/>"#),
    ));
    let mut seed: u32 = 0x9E37_79B9;
    for cut in (0..slide.len()).step_by(7) {
        let Some(prefix) = slide.get(..cut) else {
            continue;
        };
        drop(read_deck(
            Package::new(&[prefix.to_owned()]).bytes(),
            &ParseLimits::default(),
        ));
        // Swap two random bytes of the ASCII document.
        let mut bytes = slide.clone().into_bytes();
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let len = u32::try_from(bytes.len()).unwrap();
        let i = usize::try_from(seed % len).unwrap();
        let j = usize::try_from((seed >> 7) % len).unwrap();
        bytes.swap(i, j);
        let mangled = String::from_utf8_lossy(&bytes).into_owned();
        drop(read_deck(
            Package::new(&[mangled]).bytes(),
            &ParseLimits::default(),
        ));
    }
}

// --- review fixes ---------------------------------------------------------------

/// Relationships of `slide1.xml` built from `items`, read through an archive.
fn slide_rels_of(items: &[(&str, &str, &str)]) -> Relationships {
    let zip = test_zip::build(&[("ppt/slides/_rels/slide1.xml.rels", rels(items).as_bytes())]);
    let mut archive = Archive::open(zip, "PPTX", &ParseLimits::default()).unwrap();
    archive.read_rels("ppt/slides/slide1.xml").unwrap()
}

#[test]
fn repeated_smartart_references_are_queued_once() {
    let rels = slide_rels_of(&[("rIdD", "diagramData", "../diagrams/data1.xml")]);
    let inherited = Inherited::default();
    let limits = ParseLimits::default();
    let ctx = SlideContext {
        inherited: &inherited,
        rels: &rels,
        limits: &limits,
    };
    let slide = slide_xml(&diagram_frame("rIdD").repeat(500));
    let read = read_slide(&slide, &ctx, &mut Budgets::new(&limits)).unwrap();
    assert_eq!(read.diagrams.len(), 1);
}

#[test]
fn overlong_link_targets_are_plain_text() {
    let long = format!("https://example.com/{}", "a".repeat(100_000));
    let runs = link_run("x", r#"r:id="rIdH""#).repeat(50);
    let blocks = linked_slide(&runs, &[("rIdH", "hyperlink", &long)]);
    assert_eq!(blocks, [h2("Slide 1"), p(&"x".repeat(50))]);
}

#[test]
fn only_hyperlink_relationships_become_links() {
    let blocks = linked_slide(
        &link_run("img", r#"r:id="rIdI""#),
        &[("rIdI", "image", "https://example.com/a.png")],
    );
    assert_eq!(blocks, [h2("Slide 1"), p("img")]);
}

#[test]
fn placeholder_lookup_prefers_the_first_index_then_kind() {
    let mut placeholders = styles::Placeholders::default();
    let mut numbered = ListStyle::default();
    numbered.set(0, Bullet::Ordered);
    let mut off = ListStyle::default();
    off.set(0, Bullet::None);
    let ph = |xml: &str| {
        let mut reader = crate::infra::parsers::ooxml::xml::reader(xml);
        match reader.read_event().unwrap() {
            Event::Empty(e) => Placeholder::from_element(&e),
            other => panic!("unexpected {other:?}"),
        }
    };
    placeholders.insert(ph(r#"<p:ph idx="5"/>"#), numbered);
    placeholders.insert(ph(r#"<p:ph type="subTitle" idx="5"/>"#), off);
    let found = placeholders.find(&ph(r#"<p:ph idx="5"/>"#)).unwrap();
    assert_eq!(found.1.get(0), Some(Bullet::Ordered), "first idx wins");
    let by_kind = placeholders
        .find(&ph(r#"<p:ph type="subTitle"/>"#))
        .unwrap();
    assert_eq!(by_kind.1.get(0), Some(Bullet::None));
    assert!(
        placeholders.find(&ph(r#"<p:ph idx="9"/>"#)).is_none(),
        "a placeholder with only an index matches by index alone"
    );
    assert!(
        placeholders
            .find(&ph(r#"<p:ph type="title" idx="9"/>"#))
            .is_none()
    );
}

#[test]
fn case_aliases_of_a_slide_are_read_once() {
    let mut pkg = Package::new(&[slide_xml(&title_sp("Same"))]);
    pkg.set(
        "ppt/presentation.xml",
        &format!(
            r#"<p:presentation {NS}><p:sldIdLst><p:sldId r:id="a"/><p:sldId r:id="b"/><p:sldId r:id="c"/></p:sldIdLst></p:presentation>"#
        ),
    );
    pkg.set(
        "ppt/_rels/presentation.xml.rels",
        &rels(&[
            ("a", "slide", "slides/slide1.xml"),
            ("b", "slide", "slides/SLIDE1.xml"),
            ("c", "slide", "Slides/slide1.XML"),
        ]),
    );
    assert_eq!(pkg.read().blocks, [h2("Same")]);
}

#[test]
fn slide_headings_count_as_text() {
    let limits = ParseLimits {
        max_text_bytes: 20,
        ..ParseLimits::default()
    };
    let slides: Vec<String> = (0..5).map(|_| slide_xml("")).collect();
    let err = Package::new(&slides).error(&limits);
    assert!(err.contains("max_text_bytes"), "{err}");
}

#[test]
fn numbering_follows_slide_list_positions() {
    let mut pkg = Package::new(&[slide_xml(""), slide_xml("")]);
    pkg.set(
        "ppt/presentation.xml",
        &format!(
            r#"<p:presentation {NS}><p:sldIdLst><p:sldId r:id="rId404"/><p:sldId r:id="rId1"/><p:sldId r:id="rId2"/></p:sldIdLst></p:presentation>"#
        ),
    );
    assert_eq!(headings(&pkg.read().blocks), ["Slide 2", "Slide 3"]);
}

#[test]
fn smartart_breaks_count_as_text() {
    let mut pkg = Package::new(&[slide_xml(&diagram_frame("rIdD"))]);
    pkg.set(
        "ppt/slides/_rels/slide1.xml.rels",
        &rels(&[("rIdD", "diagramData", "../diagrams/data1.xml")]),
    );
    pkg.set(
        "ppt/diagrams/data1.xml",
        &format!(
            r#"<dgm:dataModel xmlns:dgm="d" xmlns:a="a"><dgm:ptLst><dgm:pt><dgm:t><a:p><a:r><a:t>x</a:t></a:r>{}<a:br></a:br><a:r><a:t>y</a:t></a:r></a:p></dgm:t></dgm:pt></dgm:ptLst></dgm:dataModel>"#,
            "<a:br/>".repeat(200)
        ),
    );
    let limits = ParseLimits {
        max_text_bytes: 100,
        ..ParseLimits::default()
    };
    let err = pkg.error(&limits);
    assert!(err.contains("max_text_bytes"), "{err}");
}

#[test]
fn malformed_optional_parts_cost_only_their_content() {
    let mut pkg = Package::new(&[
        slide_xml(&body_sp(&para("Kept"))),
        format!(
            r"<p:sld {NS}><p:cSld><p:spTree>{}</p:wrong>",
            title_sp("Broken")
        ),
        slide_xml(&format!("{}{}", title_sp("Last"), diagram_frame("rIdD"))),
    ]);
    pkg.set(
        "ppt/slideLayouts/slideLayout1.xml",
        "<p:sldLayout><p:cSld></p:oops>",
    );
    pkg.set(
        "ppt/slides/_rels/slide3.xml.rels",
        &rels(&[("rIdD", "diagramData", "../diagrams/data1.xml")]),
    );
    pkg.set(
        "ppt/diagrams/data1.xml",
        "<dgm:dataModel><a:p></dgm:dataModel>",
    );
    pkg.set(
        "ppt/slides/_rels/slide1.xml.rels",
        "<Relationships><Relationship",
    );
    assert_eq!(
        pkg.read().blocks,
        [
            h2("Slide 1"),
            p("Kept"),
            ParsedBlock::PageBreak,
            h2("Slide 2"),
            ParsedBlock::PageBreak,
            h2("Last"),
        ]
    );
}

#[test]
fn field_properties_do_not_change_the_paragraph() {
    let paras = r#"<a:p><a:pPr lvl="2"/><a:r><a:t>Page </a:t></a:r><a:fld type="slidenum"><a:rPr/><a:pPr/><a:t>3</a:t></a:fld></a:p>"#;
    assert_eq!(
        one_slide(&body_sp(paras)),
        [h2("Slide 1"), li(2, false, "Page 3")]
    );
}

#[test]
fn misplaced_hidden_flags_do_not_corrupt_state() {
    let shapes = format!(
        r#"{}<mc:AlternateContent><mc:Choice><p:cNvPr hidden="1"/></mc:Choice><mc:Fallback>{}</mc:Fallback></mc:AlternateContent>{}"#,
        table_frame(&format!(
            r#"<a:tr>{}</a:tr><p:cNvPr hidden="1"/>"#,
            tc("cell")
        )),
        text_box(&para("FALLBACK")),
        text_box(&para("After"))
    );
    let blocks = one_slide(&shapes);
    assert_eq!(blocks.len(), 3, "{blocks:?}");
    assert!(matches!(blocks[1], ParsedBlock::Table(_)), "{blocks:?}");
    assert_eq!(blocks[2], p("After"));
}

#[test]
fn hidden_pictures_and_frames_are_skipped() {
    let hidden_frame = table_frame(&format!("<a:tr>{}</a:tr>", tc("x")))
        .replace(r#"name="t"/>"#, r#"name="t" hidden="1"/>"#);
    let shapes = format!(
        "{}{}{}",
        pic(r#"<p:cNvPr id="4" name="p" descr="Secret" hidden="1"/>"#),
        hidden_frame,
        text_box(&para("Visible"))
    );
    assert_eq!(one_slide(&shapes), [h2("Slide 1"), p("Visible")]);
}

#[test]
fn alternate_content_variants() {
    let fallback_only = format!(
        "<mc:AlternateContent><mc:Fallback>{}</mc:Fallback></mc:AlternateContent>",
        text_box(&para("F"))
    );
    let nested = format!(
        "<mc:AlternateContent><mc:Choice><mc:AlternateContent><mc:Choice>{}</mc:Choice><mc:Fallback>{}</mc:Fallback></mc:AlternateContent></mc:Choice><mc:Fallback>{}</mc:Fallback></mc:AlternateContent>",
        text_box(&para("Inner")),
        text_box(&para("X1")),
        text_box(&para("X2"))
    );
    let table = format!(
        "<mc:AlternateContent><mc:Choice>{}</mc:Choice><mc:Fallback>{}</mc:Fallback></mc:AlternateContent>",
        table_frame(&format!("<a:tr>{}</a:tr>", tc("T"))),
        pic(r#"<p:cNvPr id="9" descr="table picture"/>"#)
    );
    let blocks = one_slide(&format!("{fallback_only}{nested}{table}"));
    assert_eq!(blocks[1..3], [p("F"), p("Inner")]);
    assert!(matches!(blocks[3], ParsedBlock::Table(_)), "{blocks:?}");
    assert_eq!(blocks.len(), 4, "{blocks:?}");
}

#[test]
fn subtitle_inherits_from_the_master_body_placeholder() {
    let mut pkg = Package::new(&[slide_xml(&sp(
        r#"<p:ph type="subTitle" idx="9"/>"#,
        &para("Sub"),
    ))]);
    pkg.set(
        "ppt/slideLayouts/slideLayout1.xml",
        r#"<p:sldLayout xmlns:p="p"><p:cSld><p:spTree/></p:cSld></p:sldLayout>"#,
    );
    pkg.set(
        "ppt/slideMasters/slideMaster1.xml",
        &MASTER.replace(
            r#"<p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:txBody><a:lstStyle/>"#,
            r#"<p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:txBody><a:lstStyle><a:lvl1pPr><a:buAutoNum type="arabicPeriod"/></a:lvl1pPr></a:lstStyle>"#,
        ),
    );
    assert_eq!(pkg.read().blocks, [h2("Slide 1"), li(0, true, "Sub")]);
}

#[test]
fn image_descriptions_count_as_text() {
    let limits = ParseLimits {
        max_text_bytes: 10,
        ..ParseLimits::default()
    };
    let err = Package::new(&[slide_xml(&pic(
        r#"<p:cNvPr id="4" descr="a long description"/>"#,
    ))])
    .error(&limits);
    assert!(err.contains("max_text_bytes"), "{err}");
}

#[test]
fn control_characters_are_dropped_and_returns_break_lines() {
    let blocks = one_slide(&text_box(&para("a&#1;b&#13;c&#x7f;d")));
    assert_eq!(blocks, [h2("Slide 1"), p("ab\ncd")]);
}

#[test]
fn core_properties_are_found_without_a_relationship() {
    let mut pkg = Package::new(&[]);
    pkg.set(
        "_rels/.rels",
        &rels(&[("rId1", "officeDocument", "ppt/presentation.xml")]),
    );
    pkg.set(
        "docProps/core.xml",
        r#"<cp:coreProperties xmlns:dc="d"><dc:title>Fallback</dc:title></cp:coreProperties>"#,
    );
    assert_eq!(pkg.read().title.as_deref(), Some("Fallback"));
}
