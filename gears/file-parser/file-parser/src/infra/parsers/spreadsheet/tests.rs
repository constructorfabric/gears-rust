use std::fmt::Write as _;

use super::xls::biff;
use super::*;
use crate::infra::parsers::ooxml::test_zip;

/// A small XLSX package, built part by part.
struct Book {
    /// Where `_rels/.rels` says the workbook is.
    workbook_part: String,
    sheets: Vec<(String, String)>,
    shared_strings: Option<String>,
    styles: Option<String>,
    macro_enabled: bool,
    extra: Vec<(String, Vec<u8>)>,
}

impl Book {
    fn new() -> Self {
        Self {
            workbook_part: "xl/workbook.xml".to_owned(),
            sheets: Vec::new(),
            shared_strings: None,
            styles: None,
            macro_enabled: false,
            extra: Vec::new(),
        }
    }

    fn sheet(mut self, name: &str, rows: &str) -> Self {
        self.sheets.push((name.to_owned(), rows.to_owned()));
        self
    }

    fn bytes(&self) -> Bytes {
        let dir = self
            .workbook_part
            .rsplit_once('/')
            .map_or("", |(dir, _)| dir)
            .to_owned();
        let in_dir = |name: &str| {
            if dir.is_empty() {
                name.to_owned()
            } else {
                format!("{dir}/{name}")
            }
        };

        let mut sheets_xml = String::new();
        let mut rels = String::new();
        let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
        for (i, (name, rows)) in self.sheets.iter().enumerate() {
            let n = i + 1;
            write!(
                sheets_xml,
                r#"<sheet name="{name}" sheetId="{n}" r:id="rId{n}"/>"#
            )
            .unwrap();
            write!(
                rels,
                r#"<Relationship Id="rId{n}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{n}.xml"/>"#
            )
            .unwrap();
            parts.push((
                in_dir(&format!("worksheets/sheet{n}.xml")),
                format!(r#"<worksheet xmlns="m"><sheetData>{rows}</sheetData></worksheet>"#)
                    .into_bytes(),
            ));
        }
        if let Some(sst) = &self.shared_strings {
            rels.push_str(r#"<Relationship Id="rIdS" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>"#);
            parts.push((in_dir("sharedStrings.xml"), sst.clone().into_bytes()));
        }
        if let Some(styles) = &self.styles {
            rels.push_str(r#"<Relationship Id="rIdT" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>"#);
            parts.push((in_dir("styles.xml"), styles.clone().into_bytes()));
        }

        let content_type = if self.macro_enabled {
            "application/vnd.ms-excel.sheet.macroEnabled.main+xml"
        } else {
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"
        };
        parts.push((
            "[Content_Types].xml".to_owned(),
            format!(
                r#"<Types xmlns="t"><Override PartName="/{}" ContentType="{content_type}"/></Types>"#,
                self.workbook_part
            )
            .into_bytes(),
        ));
        parts.push((
            "_rels/.rels".to_owned(),
            format!(
                r#"<Relationships xmlns="r"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="{}"/></Relationships>"#,
                self.workbook_part
            )
            .into_bytes(),
        ));
        parts.push((
            self.workbook_part.clone(),
            format!(r#"<workbook xmlns="m" xmlns:r="r"><sheets>{sheets_xml}</sheets></workbook>"#)
                .into_bytes(),
        ));
        parts.push((
            crate::infra::parsers::ooxml::rels_part_for(&self.workbook_part),
            format!(r#"<Relationships xmlns="r">{rels}</Relationships>"#).into_bytes(),
        ));
        parts.extend(self.extra.iter().cloned());

        let entries: Vec<(&str, &[u8])> = parts
            .iter()
            .map(|(name, data)| (name.as_str(), data.as_slice()))
            .collect();
        test_zip::build(&entries)
    }
}

fn inline_cell(reference: &str, text: &str) -> String {
    format!(r#"<c r="{reference}" t="inlineStr"><is><t>{text}</t></is></c>"#)
}

fn number_cell(reference: &str, value: &str) -> String {
    format!(r#"<c r="{reference}"><v>{value}</v></c>"#)
}

fn row(n: u32, cells: &[String]) -> String {
    format!(r#"<row r="{n}">{}</row>"#, cells.concat())
}

fn first_text(inlines: &[Inline]) -> Option<String> {
    match inlines.first() {
        Some(Inline::Text { text, .. }) => Some(text.clone()),
        _ => None,
    }
}

fn headings(blocks: &[ParsedBlock]) -> Vec<String> {
    blocks
        .iter()
        .filter_map(|b| match b {
            ParsedBlock::Heading { inlines, .. } => first_text(inlines),
            _ => None,
        })
        .collect()
}

fn paragraphs(blocks: &[ParsedBlock]) -> Vec<String> {
    blocks
        .iter()
        .filter_map(|b| match b {
            ParsedBlock::Paragraph { inlines } => first_text(inlines),
            _ => None,
        })
        .collect()
}

fn table_texts(table: &TableBlock) -> Vec<Vec<String>> {
    table
        .rows
        .iter()
        .map(|r| {
            r.cells
                .iter()
                .map(|c| match c.blocks.first() {
                    Some(ParsedBlock::Paragraph { inlines }) => {
                        first_text(inlines).unwrap_or_default()
                    }
                    _ => String::new(),
                })
                .collect()
        })
        .collect()
}

fn tables(blocks: &[ParsedBlock]) -> Vec<&TableBlock> {
    blocks
        .iter()
        .filter_map(|b| match b {
            ParsedBlock::Table(t) => Some(t),
            _ => None,
        })
        .collect()
}

fn read(bytes: Bytes, limits: &ParseLimits) -> Workbook {
    read_workbook(bytes, limits).expect("workbook parses")
}

fn read_err(bytes: Bytes, limits: &ParseLimits) -> String {
    read_workbook(bytes, limits)
        .err()
        .expect("workbook is rejected")
        .to_string()
}

#[test]
fn one_heading_per_sheet_and_table_over_bounding_box() {
    let sheet = [
        row(2, &[inline_cell("B2", "Name"), inline_cell("C2", "Qty")]),
        row(3, &[inline_cell("B3", "Bolt"), number_cell("C3", "10")]),
    ]
    .concat();
    let wb = read(
        Book::new()
            .sheet("Parts", &sheet)
            .sheet("Empty", "")
            .bytes(),
        &ParseLimits::default(),
    );
    assert_eq!(wb.format, Format::Xlsx);
    assert_eq!(headings(&wb.blocks), ["Parts", "Empty"]);
    let tables = tables(&wb.blocks);
    assert_eq!(tables.len(), 1, "empty sheet gets the heading only");
    assert_eq!(
        table_texts(tables[0]),
        [vec!["Name", "Qty"], vec!["Bolt", "10"]]
    );
    assert!(tables[0].rows[0].is_header);
    assert!(!tables[0].rows[1].is_header);
}

#[test]
fn shared_strings_styles_and_cell_types() {
    let mut book = Book::new().sheet(
        "S",
        &row(
            1,
            &[
                r#"<c r="A1" t="s"><v>1</v></c>"#.to_owned(),
                r#"<c r="B1" s="1"><v>45352</v></c>"#.to_owned(),
                r#"<c r="C1" t="b"><v>1</v></c>"#.to_owned(),
                r#"<c r="D1" t="e"><v>#N/A</v></c>"#.to_owned(),
                r#"<c r="E1" t="str"><f>A1</f><v>calc</v></c>"#.to_owned(),
                r#"<c r="F1"><f>1/0</f></c>"#.to_owned(),
                r#"<c r="G1"><v>0.30000000000000004</v></c>"#.to_owned(),
                r#"<c r="H1" t="s"><v>99</v></c>"#.to_owned(),
            ],
        ),
    );
    book.shared_strings = Some(
        r#"<sst uniqueCount="1000000000000000"><si><t>zero</t></si><si><t>one</t></si></sst>"#
            .to_owned(),
    );
    book.styles = Some(
        r#"<styleSheet><cellXfs><xf numFmtId="0"/><xf numFmtId="14"/></cellXfs></styleSheet>"#
            .to_owned(),
    );
    let wb = read(book.bytes(), &ParseLimits::default());
    // A formula without a cached value and a dangling string index show
    // nothing, so the bounding box ends at G.
    assert_eq!(
        table_texts(tables(&wb.blocks)[0]),
        [vec!["one", "2024-03-01", "TRUE", "#N/A", "calc", "", "0.3"]]
    );
}

#[test]
fn huge_sparse_sheet_is_skipped_not_allocated() {
    // A1 and XFD1048576: two cells whose bounding box is 17 billion slots.
    let sheet = [
        row(1, &[inline_cell("A1", "first")]),
        row(1_048_576, &[inline_cell("XFD1048576", "last")]),
    ]
    .concat();
    let small = row(1, &[inline_cell("A1", "kept")]);
    let wb = read(
        Book::new()
            .sheet("Wide", &sheet)
            .sheet("Small", &small)
            .bytes(),
        &ParseLimits::default(),
    );
    assert_eq!(headings(&wb.blocks), ["Wide", "Small"]);
    let tables = tables(&wb.blocks);
    assert_eq!(tables.len(), 1);
    assert_eq!(table_texts(tables[0]), [vec!["kept"]]);
    assert!(
        paragraphs(&wb.blocks)
            .iter()
            .any(|p| p.contains("\"Wide\" skipped")),
        "skipped sheet must be announced: {:?}",
        wb.blocks
    );
}

#[test]
fn budget_spans_sheets_and_announces_the_skip() {
    let three = row(
        1,
        &[
            number_cell("A1", "1"),
            number_cell("B1", "2"),
            number_cell("C1", "3"),
        ],
    );
    let limits = ParseLimits {
        max_table_cells: 4,
        ..ParseLimits::default()
    };
    let wb = read(
        Book::new()
            .sheet("One", &three)
            .sheet("Two", &three)
            .bytes(),
        &limits,
    );
    assert_eq!(tables(&wb.blocks).len(), 1, "second sheet no longer fits");
    assert!(
        paragraphs(&wb.blocks)
            .iter()
            .any(|p| p.contains("\"Two\" skipped"))
    );
}

#[test]
fn one_shared_string_in_many_cells_is_held_to_the_text_limit() {
    let mut book = Book::new().sheet(
        "S",
        &(1..=50)
            .map(|r| row(r, &[format!(r#"<c r="A{r}" t="s"><v>0</v></c>"#)]))
            .collect::<String>(),
    );
    book.shared_strings = Some(format!("<sst><si><t>{}</t></si></sst>", "x".repeat(10_000)));
    let limits = ParseLimits {
        max_text_bytes: 100_000,
        ..ParseLimits::default()
    };
    // 50 copies x 10 000 bytes = 500 000 > 100 000.
    assert!(read_err(book.bytes(), &limits).contains("max_text_bytes"));
}

#[test]
fn relocated_parts_are_found_and_still_limited() {
    // The workbook and its sheets live under xl/media/, as a hostile file may
    // arrange; they are read through relationships and the ZIP limits apply.
    let mut book = Book::new().sheet("S", &row(1, &[number_cell("A1", "7")]));
    book.workbook_part = "xl/media/workbook.xml".to_owned();
    let wb = read(book.bytes(), &ParseLimits::default());
    assert_eq!(table_texts(tables(&wb.blocks)[0]), [vec!["7"]]);

    let mut bomb = Book::new().sheet("S", &" ".repeat(2 * 1024 * 1024));
    bomb.workbook_part = "xl/media/workbook.xml".to_owned();
    assert!(read_err(bomb.bytes(), &ParseLimits::default()).contains("max_compression_ratio"));
}

#[test]
fn compressible_media_does_not_reject_the_workbook() {
    let mut book = Book::new().sheet("S", &row(1, &[number_cell("A1", "1")]));
    book.extra
        .push(("xl/media/image1.bmp".to_owned(), vec![0; 2 * 1024 * 1024]));
    let wb = read(book.bytes(), &ParseLimits::default());
    assert_eq!(tables(&wb.blocks).len(), 1);
}

#[test]
fn macro_enabled_workbook_reports_the_xlsm_type() {
    let mut book = Book::new().sheet("S", &row(1, &[number_cell("A1", "1")]));
    book.macro_enabled = true;
    let wb = read(book.bytes(), &ParseLimits::default());
    assert_eq!(wb.format, Format::Xlsm);
    assert_eq!(
        wb.format.mime(),
        "application/vnd.ms-excel.sheet.macroEnabled.12"
    );
}

#[test]
fn format_detected_from_content_not_name() {
    let parser = SpreadsheetParser::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let bytes = Book::new()
        .sheet("S", &row(1, &[number_cell("A1", "1")]))
        .bytes();
    let doc = runtime
        .block_on(parser.parse_bytes(Some("book.xls"), None, bytes))
        .unwrap();
    assert_eq!(
        doc.meta.content_type.as_deref(),
        Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
    );
}

#[test]
fn zip_package_without_workbook_is_rejected() {
    let bytes = test_zip::build(&[("word/document.xml", b"<w:document/>")]);
    assert!(read_err(bytes, &ParseLimits::default()).contains("no workbook part"));
}

#[test]
fn legacy_xls_round_trips() {
    let stream = biff::workbook(
        "Legacy",
        &[biff::number(0, 0, 1.5), biff::number(1, 1, 42.0)],
    );
    let wb = read(Bytes::from(biff::xls(&stream)), &ParseLimits::default());
    assert_eq!(wb.format, Format::Xls);
    assert_eq!(headings(&wb.blocks), ["Legacy"]);
    assert_eq!(
        table_texts(tables(&wb.blocks)[0]),
        [vec!["1.5", ""], vec!["", "42"]]
    );
}

#[test]
fn legacy_xls_declared_dimensions_are_ignored() {
    // DIMENSIONS claiming 4 billion rows: nothing is sized from it.
    let mut dims = 0_u32.to_le_bytes().to_vec();
    dims.extend_from_slice(&u32::MAX.to_le_bytes());
    dims.extend_from_slice(&[0, 0, 0, 1, 0, 0]);
    let stream = biff::workbook("S", &[biff::record(0x0200, &dims), biff::number(0, 0, 3.0)]);
    let wb = read(Bytes::from(biff::xls(&stream)), &ParseLimits::default());
    assert_eq!(table_texts(tables(&wb.blocks)[0]), [vec!["3"]]);
}

#[test]
fn legacy_xls_far_apart_cells_skip_the_sheet() {
    // A1 and IV65536: the box does not fit the cell budget, nothing dense is
    // allocated, the sheet is announced as skipped.
    let stream = biff::workbook(
        "Wide",
        &[biff::number(0, 0, 1.0), biff::number(65_535, 255, 2.0)],
    );
    let wb = read(Bytes::from(biff::xls(&stream)), &ParseLimits::default());
    assert!(tables(&wb.blocks).is_empty());
    assert!(
        paragraphs(&wb.blocks)
            .iter()
            .any(|p| p.contains("\"Wide\" skipped"))
    );
}

#[test]
fn legacy_xls_cell_kinds() {
    let rk_int = |n: i32| u32::from_le_bytes((n << 2).to_le_bytes()) | 0x02;
    let str_result = [0, 0, 0, 0, 0, 0, 0xFF, 0xFF];
    let bool_result = [1, 0, 1, 0, 0, 0, 0xFF, 0xFF];
    let err_result = [2, 0, 0x07, 0, 0, 0, 0xFF, 0xFF];
    let stream = biff::workbook_with(
        &[
            biff::format(164, "yyyy-mm-dd"),
            biff::xf(0),
            biff::xf(164),
            biff::sst(&["shared", "other"]),
        ],
        &[(
            "Kinds",
            vec![
                biff::rk(0, 0, rk_int(7)),
                biff::label_sst(0, 1, 1),
                biff::label(0, 2, "\u{43b}abel"),
                biff::boolerr(0, 3, 1, false),
                biff::boolerr(0, 4, 0x2A, true),
                biff::number_xf(0, 5, 1, 45_352.0),
                biff::formula(0, 6, str_result, 40_000),
                biff::formula_string("calc"),
                biff::formula(0, 7, bool_result, 3),
                biff::formula(0, 8, err_result, 3),
                biff::formula(0, 9, 2.5_f64.to_le_bytes(), 3),
                biff::mulrk(1, 0, &[rk_int(1), rk_int(2)]),
            ],
        )],
    );
    let wb = read(Bytes::from(biff::xls(&stream)), &ParseLimits::default());
    let table = table_texts(tables(&wb.blocks)[0]);
    assert_eq!(
        table[0],
        [
            "7",
            "other",
            "\u{43b}abel",
            "TRUE",
            "#N/A",
            "2024-03-01",
            "calc",
            "TRUE",
            "#DIV/0!",
            "2.5"
        ]
    );
    assert_eq!(table[1][..2], ["1", "2"]);
}

#[test]
fn legacy_xls_hidden_stream_and_encryption_are_rejected() {
    let stream = biff::workbook("S", &[biff::number(0, 0, 1.0)]);
    let hidden = read_err(
        Bytes::from(biff::xls_at("/X/Workbook", &stream)),
        &ParseLimits::default(),
    );
    assert!(hidden.contains("no Workbook stream"), "{hidden}");

    let encrypted = biff::workbook_with(&[biff::record(0x002F, &[0; 6])], &[("S", vec![])]);
    let err = read_err(Bytes::from(biff::xls(&encrypted)), &ParseLimits::default());
    assert!(err.contains("encrypted"), "{err}");
}

#[test]
fn legacy_xls_too_many_sheets() {
    let stream = biff::workbook_with(&[], &[("A", vec![]), ("B", vec![])]);
    let limits = ParseLimits {
        max_sheets: 1,
        ..ParseLimits::default()
    };
    assert!(read_err(Bytes::from(biff::xls(&stream)), &limits).contains("max_sheets"));
}

#[test]
fn unrecognized_bytes_are_rejected() {
    assert!(
        read_err(Bytes::from_static(b"plain text"), &ParseLimits::default())
            .contains("not an Excel workbook")
    );
}

#[test]
fn xlsb_workbook_with_shared_strings_styles_and_short_records() {
    use super::xlsb::build;

    let workbook = build::workbook(&[("rId1", "Numbers")], false);
    let mut sheet = build::row(0);
    sheet.extend(build::real(0, 0, 1.5));
    sheet.extend(build::isst(1, 0));
    sheet.extend(build::real(2, 1, 45_352.0));
    sheet.extend(build::boolean(3, true));
    // An empty BrtCellSt must not repeat earlier bytes.
    sheet.extend(build::record(0x0006, &[]));
    sheet.extend(build::row(1));
    sheet.extend(build::text(1, "second"));
    let package = build::package(
        &workbook,
        &[("rId1", "worksheets/sheet1.bin", sheet)],
        Some(build::sst(&["shared"])),
        Some(build::styles(&[(164, "yyyy-mm-dd")], &[0, 164])),
    );
    let wb = read(package, &ParseLimits::default());
    assert_eq!(wb.format, Format::Xlsb);
    assert_eq!(headings(&wb.blocks), ["Numbers"]);
    assert_eq!(
        table_texts(tables(&wb.blocks)[0]),
        [
            vec!["1.5", "shared", "2024-03-01", "TRUE"],
            vec!["", "second", "", ""]
        ]
    );
}

#[test]
fn xlsb_oversized_record_is_rejected() {
    use super::xlsb::build;

    let workbook = build::workbook(&[("rId1", "S")], false);
    let evil = vec![0x05, 0xFF, 0xFF, 0xFF, 0x7F];
    let package = build::package(
        &workbook,
        &[("rId1", "worksheets/sheet1.dat", evil)],
        None,
        None,
    );
    assert!(read_err(package, &ParseLimits::default()).contains("runs past the part"));
}

#[test]
fn text_of_a_skipped_sheet_is_given_back() {
    // The first sheet is too wide for the cell budget and is skipped; its text
    // must not use up the text budget of the sheets after it.
    let wide = row(
        1,
        &[
            inline_cell("A1", &"x".repeat(600)),
            inline_cell("Z1", "far"),
        ],
    );
    let small = row(1, &[inline_cell("A1", &"y".repeat(600))]);
    let limits = ParseLimits {
        max_table_cells: 10,
        max_text_bytes: 1_000,
        ..ParseLimits::default()
    };
    let wb = read(
        Book::new()
            .sheet("Wide", &wide)
            .sheet("Small", &small)
            .bytes(),
        &limits,
    );
    assert_eq!(tables(&wb.blocks).len(), 1);
    assert!(
        paragraphs(&wb.blocks)
            .iter()
            .any(|p| p.contains("\"Wide\" skipped"))
    );
}

#[test]
fn too_many_sheets_are_rejected() {
    let mut book = Book::new();
    for i in 0..3 {
        book = book.sheet(&format!("S{i}"), "");
    }
    let limits = ParseLimits {
        max_sheets: 2,
        ..ParseLimits::default()
    };
    assert!(read_err(book.bytes(), &limits).contains("max_sheets"));
}

/// A BIFF5 workbook with a Book stream: code page record, byte-string sheet
/// name and LABEL.
#[test]
fn biff5_workbook_uses_its_code_page() {
    let rec = biff::record;
    let bof5 = |dt: u16| {
        let mut body = 0x0500_u16.to_le_bytes().to_vec();
        body.extend_from_slice(&dt.to_le_bytes());
        body.extend_from_slice(&[0; 4]);
        rec(0x0809, &body)
    };
    let codepage = rec(0x0042, &1251_u16.to_le_bytes());
    let name = [0xCB_u8, 0xE8, 0xF1, 0xF2]; // "Лист" in Windows-1251
    let boundsheet_len = 4 + 4 + 2 + 1 + name.len();
    let globals_len = bof5(5).len() + codepage.len() + boundsheet_len + 4;
    let mut boundsheet = u32::try_from(globals_len).unwrap().to_le_bytes().to_vec();
    boundsheet.extend_from_slice(&[0, 0, u8::try_from(name.len()).unwrap()]);
    boundsheet.extend_from_slice(&name);
    let mut label = vec![0, 0, 0, 0, 0, 0];
    label.extend_from_slice(&2_u16.to_le_bytes());
    label.extend_from_slice(&[0xC4, 0xE0]); // "Да"

    let mut stream = bof5(5);
    stream.extend(codepage);
    stream.extend(rec(0x0085, &boundsheet));
    stream.extend(rec(0x000A, &[]));
    assert_eq!(stream.len(), globals_len);
    stream.extend(bof5(0x10));
    stream.extend(rec(0x0204, &label));
    stream.extend(rec(0x000A, &[]));

    let wb = read(
        Bytes::from(biff::xls_at("/Book", &stream)),
        &ParseLimits::default(),
    );
    assert_eq!(headings(&wb.blocks), ["\u{41b}\u{438}\u{441}\u{442}"]);
    assert_eq!(table_texts(tables(&wb.blocks)[0]), [vec!["\u{414}\u{430}"]]);
}

#[test]
fn xls_chart_substreams_and_chart_sheets_are_skipped() {
    // A worksheet with an embedded chart substream, followed by a chart sheet
    // whose offset bounds the worksheet walk.
    let chart_sheet = |offset: u32| {
        let mut body = offset.to_le_bytes().to_vec();
        body.extend_from_slice(&[0, 2, 1, 0, b'C']);
        biff::record(0x0085, &body)
    };
    let sheet = biff::workbook_with(
        &[],
        &[(
            "Data",
            vec![
                biff::number(0, 0, 1.0),
                biff::bof(true),
                biff::number(500, 200, 9.0),
                biff::eof(),
                biff::number(1, 0, 2.0),
            ],
        )],
    );
    // Splice a chart-sheet BOUNDSHEET into the globals, pointing past the end.
    let globals_end = sheet
        .windows(4)
        .position(|w| w == [0x0A, 0x00, 0x00, 0x00])
        .unwrap();
    let mut stream = sheet[..globals_end].to_vec();
    let extra = chart_sheet(0);
    let shift = extra.len();
    stream.extend(chart_sheet(u32::try_from(sheet.len() + shift).unwrap()));
    stream.extend_from_slice(&sheet[globals_end..]);
    // The worksheet offset moved by the inserted record.
    let data_offset_at = stream.windows(2).position(|w| w == [0x85, 0x00]).unwrap() + 4;
    let old = u32::from_le_bytes(
        stream[data_offset_at..data_offset_at + 4]
            .try_into()
            .unwrap(),
    );
    let new = old + u32::try_from(shift).unwrap();
    stream[data_offset_at..data_offset_at + 4].copy_from_slice(&new.to_le_bytes());
    stream.extend(biff::bof(true));
    stream.extend(biff::number(0, 0, 77.0));
    stream.extend(biff::eof());

    let wb = read(Bytes::from(biff::xls(&stream)), &ParseLimits::default());
    assert_eq!(
        headings(&wb.blocks),
        ["Data"],
        "the chart sheet holds no cells"
    );
    assert_eq!(table_texts(tables(&wb.blocks)[0]), [vec!["1"], vec!["2"]]);
}

#[test]
fn xls_1904_dates_empty_formula_results_and_strings() {
    let str_result = [0, 0, 0, 0, 0, 0, 0xFF, 0xFF];
    let empty_result = [3, 0, 0, 0, 0, 0, 0xFF, 0xFF];
    // A long formula result split over a STRING record and a CONTINUE.
    let long = "y".repeat(300);
    let mut string_body = 300_u16.to_le_bytes().to_vec();
    string_body.push(0);
    string_body.extend_from_slice(&long.as_bytes()[..100]);
    let mut cont = vec![0];
    cont.extend_from_slice(&long.as_bytes()[100..]);
    let stream = biff::workbook_with(
        &[
            biff::record(0x0022, &1_u16.to_le_bytes()), // DATEMODE: 1904
            biff::xf(0),
            biff::xf(14),
        ],
        &[(
            "S",
            vec![
                biff::number_xf(0, 0, 1, 1.0),
                biff::formula(0, 1, empty_result, 3),
                biff::formula(0, 2, str_result, 3),
                // An intervening shared-formula record does not drop the
                // pending string result.
                biff::record(0x04BC, &[0; 8]),
                biff::formula_string("after"),
                biff::formula(0, 3, str_result, 3),
                biff::record(0x0207, &string_body),
                biff::record(0x003C, &cont),
            ],
        )],
    );
    let wb = read(Bytes::from(biff::xls(&stream)), &ParseLimits::default());
    let table = table_texts(tables(&wb.blocks)[0]);
    assert_eq!(table[0][0], "1904-01-02");
    assert_eq!(table[0][1], "", "type 3 is an empty string");
    assert_eq!(table[0][2], "after");
    assert_eq!(table[0][3], long);
}

#[test]
fn xlsx_cell_without_style_uses_the_first_cell_format() {
    let mut book = Book::new().sheet("S", &row(1, &[number_cell("A1", "45352")]));
    book.styles =
        Some(r#"<styleSheet><cellXfs><xf numFmtId="14"/></cellXfs></styleSheet>"#.to_owned());
    let wb = read(book.bytes(), &ParseLimits::default());
    assert_eq!(table_texts(tables(&wb.blocks)[0]), [vec!["2024-03-01"]]);
}

#[test]
fn xlsb_is_recognised_by_its_main_part_wherever_it_is() {
    use super::xlsb::build;

    let workbook = build::workbook(&[("rId1", "S")], false);
    let mut sheet = build::row(0);
    sheet.extend(build::real(0, 0, 4.0));
    let rels = r#"<Relationships xmlns="r"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="s1.bin"/></Relationships>"#;
    let root = r#"<Relationships xmlns="r"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="bin/book.bin"/></Relationships>"#;
    let package = test_zip::build(&[
        ("_rels/.rels", root.as_bytes()),
        ("bin/book.bin", &workbook),
        ("bin/_rels/book.bin.rels", rels.as_bytes()),
        ("bin/s1.bin", &sheet),
    ]);
    let wb = read(package, &ParseLimits::default());
    assert_eq!(wb.format, Format::Xlsb);
    assert_eq!(table_texts(tables(&wb.blocks)[0]), [vec!["4"]]);
}

#[test]
fn copying_through_skipped_sheets_is_bounded() {
    // Every sheet copies a long shared string and is then skipped for its
    // width; refunds keep the text budget, the copy ceiling still stops it.
    let mut book = Book::new();
    for i in 0..20 {
        book = book.sheet(
            &format!("S{i}"),
            &row(
                1,
                &[
                    r#"<c r="A1" t="s"><v>0</v></c>"#.to_owned(),
                    inline_cell("XFD1", "far"),
                ],
            ),
        );
    }
    book.shared_strings = Some(format!("<sst><si><t>{}</t></si></sst>", "z".repeat(1_000)));
    let limits = ParseLimits {
        max_table_columns: 100,
        max_text_bytes: 5_000,
        ..ParseLimits::default()
    };
    let err = read_err(book.bytes(), &limits);
    assert!(err.contains("skipped sheets included"), "{err}");
}
