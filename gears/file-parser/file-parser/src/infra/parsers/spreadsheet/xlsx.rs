//! Streaming reader for XLSX and XLSM workbooks.
//!
//! Parts are found through the package relationships, never by fixed path,
//! and every part is read through [`Archive`], so the ZIP limits apply to all
//! of them. Nothing is sized from counts the file declares (`uniqueCount`,
//! `<dimension>`): shared strings and cells are collected as they are met.

use quick_xml::events::{BytesStart, Event};

use crate::domain::error::{DomainError, ParseLimit};
use crate::infra::parsers::limits::{ParseLimits, limit_exceeded};
use crate::infra::parsers::ooxml::{Archive, Events, xml};

use super::cells::{bool_text, iso_datetime};
use super::shared::{CellFormats, SharedStrings, number_text};
use super::{Sink, Visibility};

/// Workbook facts that do not come from the sheets.
pub(super) struct XlsxInfo {
    pub(super) macro_enabled: bool,
}

fn parse_error(detail: impl std::fmt::Display) -> DomainError {
    DomainError::parse_error(format!("Failed to parse XLSX: {detail}"))
}

/// Reads every worksheet of the package, whose workbook is `workbook_part`,
/// into `sink`.
pub(super) fn read(
    mut archive: Archive,
    workbook_part: &str,
    limits: &ParseLimits,
    sink: &mut Sink,
) -> Result<XlsxInfo, DomainError> {
    let macro_enabled = workbook_content_type(&mut archive, workbook_part)?
        .is_some_and(|ct| ct.contains("macroEnabled"));

    let workbook_xml = archive
        .read_xml(workbook_part)?
        .ok_or_else(|| parse_error("no workbook part"))?;
    let workbook = parse_workbook(&workbook_xml, limits)?;
    drop(workbook_xml);
    let rels = archive.read_rels(workbook_part)?;

    let shared = match rels.internal("sharedStrings") {
        Some(target) => match archive.read_xml(&target)? {
            Some(text) => parse_shared_strings(&text, limits)?,
            None => SharedStrings::default(),
        },
        None => SharedStrings::default(),
    };
    let styles = match rels.internal("styles") {
        Some(target) => match archive.read_xml(&target)? {
            Some(text) => parse_styles(&text, limits)?,
            None => CellFormats::default(),
        },
        None => CellFormats::default(),
    };
    let context = SheetContext {
        shared: &shared,
        styles: &styles,
        date1904: workbook.date1904,
        limits,
    };

    if workbook.sheets.len() > limits.max_sheets {
        return Err(limit_exceeded(
            ParseLimit::MaxSheets,
            format!("more than {} sheets", limits.max_sheets),
        ));
    }
    for (name, rel_id, visibility) in &workbook.sheets {
        // Chart, dialog and macro sheets hold no cells.
        let Some(rel) = rels
            .get(rel_id)
            .filter(|r| !r.external && r.is_type("worksheet"))
        else {
            continue;
        };
        let target = rel.target.clone();
        let Some(sheet_xml) = archive.read_xml(&target)? else {
            continue;
        };
        let mut cells = sink.new_sheet();
        parse_sheet(&sheet_xml, &context, sink, &mut cells)?;
        sink.finish_sheet(name, *visibility, cells)?;
    }

    Ok(XlsxInfo { macro_enabled })
}

/// The content type `[Content_Types].xml` gives the workbook part.
fn workbook_content_type(
    archive: &mut Archive,
    workbook_part: &str,
) -> Result<Option<String>, DomainError> {
    let Some(text) = archive.read_xml("[Content_Types].xml")? else {
        return Ok(None);
    };
    let wanted = format!("/{workbook_part}");
    let limits = archive.limits().clone();
    let mut events = Events::new(&text, &limits, "XLSX");
    loop {
        match events.next()? {
            Event::Start(e) | Event::Empty(e) if xml::local_name(&e) == "Override" => {
                let part = xml::attr(&e, "PartName").unwrap_or_default();
                if part.eq_ignore_ascii_case(&wanted) {
                    return Ok(xml::attr(&e, "ContentType"));
                }
            }
            Event::Eof => return Ok(None),
            _ => {}
        }
    }
}

struct WorkbookXml {
    /// (sheet name, relationship id, visibility), in workbook order.
    sheets: Vec<(String, String, Visibility)>,
    date1904: bool,
}

fn parse_workbook(text: &str, limits: &ParseLimits) -> Result<WorkbookXml, DomainError> {
    let mut events = Events::new(text, limits, "XLSX");
    let mut sheets = Vec::new();
    let mut date1904 = false;
    loop {
        match events.next()? {
            Event::Start(e) | Event::Empty(e) => match xml::local_name(&e) {
                "workbookPr" => {
                    date1904 = matches!(xml::attr(&e, "date1904").as_deref(), Some("1" | "true"));
                }
                "sheet" => {
                    if let (Some(name), Some(id)) =
                        (xml::attr(&e, "name"), xml::prefixed_attr(&e, "id"))
                    {
                        let visibility = match xml::attr(&e, "state").as_deref() {
                            Some("hidden") => Visibility::Hidden,
                            Some("veryHidden") => Visibility::VeryHidden,
                            _ => Visibility::Visible,
                        };
                        sheets.push((name, id, visibility));
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(WorkbookXml { sheets, date1904 })
}

/// Collects the text of `<t>` elements below the current element, skipping
/// phonetic runs (`<rPh>`), until the element named `end` closes.
fn rich_text(events: &mut Events<'_>, end: &str) -> Result<String, DomainError> {
    let mut text = String::new();
    let mut in_t = false;
    let mut phonetic = 0_usize;
    loop {
        match events.next()? {
            Event::Start(e) => match xml::local_name(&e) {
                "t" => in_t = true,
                "rPh" => phonetic += 1,
                _ => {}
            },
            Event::End(e) => match e.local_name().as_ref() {
                "t" => in_t = false,
                "rPh" => phonetic = phonetic.saturating_sub(1),
                name if name == end => break,
                _ => {}
            },
            Event::Text(t) if in_t && phonetic == 0 => text.push_str(&t.xml10_content()),
            Event::CData(t) if in_t && phonetic == 0 => {
                text.push_str(&t.xml10_content());
            }
            Event::GeneralRef(r) if in_t && phonetic == 0 => {
                text.push_str(&xml::resolve_ref(&r).unwrap_or_default());
            }
            Event::Eof => return Err(parse_error(format!("unterminated <{end}>"))),
            _ => {}
        }
    }
    Ok(decode_xstring(&text))
}

/// The shared string table. Its size follows the part, not `uniqueCount`.
fn parse_shared_strings(text: &str, limits: &ParseLimits) -> Result<SharedStrings, DomainError> {
    let mut events = Events::new(text, limits, "XLSX");
    let mut strings = SharedStrings::default();
    loop {
        match events.next()? {
            Event::Start(e) if xml::local_name(&e) == "si" => {
                strings.push(&rich_text(&mut events, "si")?)?;
            }
            Event::Empty(e) if xml::local_name(&e) == "si" => strings.push("")?,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(strings)
}

/// Display kind per cell style index (`<cellXfs>` order).
fn parse_styles(text: &str, limits: &ParseLimits) -> Result<CellFormats, DomainError> {
    let mut events = Events::new(text, limits, "XLSX");
    let mut formats = CellFormats::default();
    let mut in_cell_xfs = false;
    loop {
        match events.next()? {
            Event::Start(e) | Event::Empty(e) => match xml::local_name(&e) {
                "numFmt" => {
                    if let (Some(id), Some(code)) = (
                        xml::attr(&e, "numFmtId").and_then(|v| v.parse().ok()),
                        xml::attr(&e, "formatCode"),
                    ) {
                        formats.add_format(id, &code);
                    }
                }
                "cellXfs" => in_cell_xfs = true,
                "xf" if in_cell_xfs => {
                    let id: u32 = xml::attr(&e, "numFmtId")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    formats.add_xf(id);
                }
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == "cellXfs" => in_cell_xfs = false,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(formats)
}

struct SheetContext<'a> {
    shared: &'a SharedStrings,
    styles: &'a CellFormats,
    date1904: bool,
    limits: &'a ParseLimits,
}

/// One `<c>` element as read from the sheet.
#[derive(Default)]
struct RawCell {
    kind: Option<String>,
    style: Option<usize>,
    value: Option<String>,
    inline: Option<String>,
}

fn parse_sheet(
    text: &str,
    ctx: &SheetContext<'_>,
    sink: &mut Sink,
    cells: &mut super::SheetCells,
) -> Result<(), DomainError> {
    let mut events = Events::new(text, ctx.limits, "XLSX");
    // 0-based position of the last row and cell seen, for elements without `r`.
    let mut row: Option<u32> = None;
    let mut col: Option<u32> = None;
    loop {
        match events.next()? {
            Event::Start(e) | Event::Empty(e) if xml::local_name(&e) == "row" => {
                row = Some(row_position(&e, row));
                col = None;
            }
            Event::Empty(e) if xml::local_name(&e) == "c" => {
                // A cell without content still takes a position.
                col = Some(cell_position(&e, row, col).1);
            }
            Event::Start(e) if xml::local_name(&e) == "c" => {
                let (r, c) = cell_position(&e, row, col);
                row = Some(r);
                col = Some(c);
                let raw = read_cell(&e, &mut events)?;
                if let Some(text) = display_text(raw, ctx)
                    && !sink.push(cells, r, c, text)?
                {
                    return Ok(());
                }
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

fn read_cell(start: &BytesStart<'_>, events: &mut Events<'_>) -> Result<RawCell, DomainError> {
    let mut raw = RawCell {
        kind: xml::attr(start, "t"),
        style: xml::attr(start, "s").and_then(|v| v.parse().ok()),
        ..RawCell::default()
    };
    let mut in_v = false;
    loop {
        match events.next()? {
            Event::Start(e) => match xml::local_name(&e) {
                "v" => in_v = true,
                "is" => raw.inline = Some(rich_text(events, "is")?),
                _ => {}
            },
            Event::End(e) => match e.local_name().as_ref() {
                "v" => in_v = false,
                "c" => return Ok(raw),
                _ => {}
            },
            Event::Text(t) if in_v => raw
                .value
                .get_or_insert_default()
                .push_str(&t.xml10_content()),
            Event::GeneralRef(r) if in_v => raw
                .value
                .get_or_insert_default()
                .push_str(&xml::resolve_ref(&r).unwrap_or_default()),
            Event::Eof => return Err(parse_error("unterminated <c>")),
            _ => {}
        }
    }
}

/// What Excel shows for the cell, or `None` when it shows nothing.
fn display_text(raw: RawCell, ctx: &SheetContext<'_>) -> Option<String> {
    let text = match raw.kind.as_deref() {
        Some("s") => {
            let index: usize = raw.value?.trim().parse().ok()?;
            ctx.shared.get(index)?.to_owned()
        }
        Some("inlineStr") => raw.inline?,
        Some("str") => decode_xstring(&raw.value?),
        Some("b") => bool_text(raw.value?.trim() == "1").to_owned(),
        Some("e") => raw.value?,
        Some("d") => iso_datetime(raw.value?.trim()),
        // "n" or absent: a number, displayed through its style.
        _ => {
            let value = raw.value?;
            match value.trim().parse::<f64>() {
                // A cell without `s` uses the first cell format, as in Excel.
                Ok(number) => number_text(
                    number,
                    ctx.styles.kind(raw.style.unwrap_or(0)),
                    ctx.date1904,
                ),
                Err(_) => value,
            }
        }
    };
    (!text.is_empty()).then_some(text)
}

fn row_position(e: &BytesStart<'_>, previous: Option<u32>) -> u32 {
    xml::attr(e, "r")
        .and_then(|r| r.trim().parse::<u32>().ok())
        .and_then(|r| r.checked_sub(1))
        .unwrap_or_else(|| previous.map_or(0, |p| p.saturating_add(1)))
}

/// 0-based (row, column) of a `<c>`, from its `r` reference (`B7`) or, without
/// one, the next column of the current row.
fn cell_position(e: &BytesStart<'_>, row: Option<u32>, col: Option<u32>) -> (u32, u32) {
    xml::attr(e, "r")
        .and_then(|r| parse_reference(&r))
        .unwrap_or_else(|| (row.unwrap_or(0), col.map_or(0, |c| c.saturating_add(1))))
}

/// `B7` -> (6, 1). Columns run to `XFD` (16384) and rows to 1048576.
fn parse_reference(reference: &str) -> Option<(u32, u32)> {
    let reference = reference.trim();
    let split = reference.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = reference.split_at(split);
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    let mut col: u32 = 0;
    for c in letters.chars() {
        if !c.is_ascii_alphabetic() {
            return None;
        }
        col = col * 26 + (u32::from(c.to_ascii_uppercase()) - u32::from('A') + 1);
    }
    let row: u32 = digits.parse().ok()?;
    if !(1..=16_384).contains(&col) || !(1..=1_048_576).contains(&row) {
        return None;
    }
    Some((row - 1, col - 1))
}

/// Undoes the `_xHHHH_` escapes OOXML uses for characters XML cannot carry
/// (`_x000D_` is a carriage return, `_x005F_` a literal underscore).
fn decode_xstring(text: &str) -> String {
    if !text.contains("_x") {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("_x") {
        out.push_str(&rest[..at]);
        let candidate = &rest[at..];
        let decoded = candidate
            .get(2..6)
            .filter(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .filter(|_| candidate.as_bytes().get(6) == Some(&b'_'))
            .and_then(|hex| u32::from_str_radix(hex, 16).ok())
            .and_then(char::from_u32);
        if let Some(c) = decoded {
            out.push(c);
            rest = &candidate[7..];
        } else {
            out.push_str("_x");
            rest = &candidate[2..];
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::super::numfmt::Kind;
    use super::*;

    #[test]
    fn references() {
        assert_eq!(parse_reference("A1"), Some((0, 0)));
        assert_eq!(parse_reference("B7"), Some((6, 1)));
        assert_eq!(parse_reference("xfd1048576"), Some((1_048_575, 16_383)));
        assert_eq!(parse_reference("XFE1"), None);
        assert_eq!(parse_reference("A0"), None);
        assert_eq!(parse_reference("A1048577"), None);
        assert_eq!(parse_reference("AAAA1"), None);
        assert_eq!(parse_reference("1A"), None);
    }

    #[test]
    fn xstring_escapes() {
        assert_eq!(decode_xstring("a_x000D_b"), "a\rb");
        assert_eq!(decode_xstring("_x005F_x000D_"), "_x000D_");
        assert_eq!(decode_xstring("no_xescape_x12"), "no_xescape_x12");
        assert_eq!(
            decode_xstring("_xD800_"),
            "_xD800_",
            "lone surrogate stays literal"
        );
    }

    #[test]
    fn shared_strings_ignore_unique_count_and_phonetics() {
        let xml = r#"<sst xmlns="x" count="9" uniqueCount="1000000000000000">
<si><t>plain</t></si>
<si><r><t>ri</t></r><r><rPr><b/></rPr><t xml:space="preserve">ch </t></r><rPh sb="0" eb="1"><t>PHONETIC</t></rPh></si>
<si/>
<si><t>a &amp; b_x000A_c</t></si></sst>"#;
        let strings = parse_shared_strings(xml, &ParseLimits::default()).unwrap();
        assert_eq!(strings.to_vec(), ["plain", "rich ", "", "a & b\nc"]);
        assert_eq!(strings.get(4), None);
    }

    #[test]
    fn styles_map_cell_xfs_only() {
        let xml = r#"<styleSheet>
<numFmts><numFmt numFmtId="164" formatCode="yyyy-mm-dd"/><numFmt numFmtId="165" formatCode="0.0"/></numFmts>
<cellStyleXfs><xf numFmtId="14"/></cellStyleXfs>
<cellXfs><xf numFmtId="0"/><xf numFmtId="164"/><xf numFmtId="165"/><xf numFmtId="22"/></cellXfs>
</styleSheet>"#;
        let formats = parse_styles(xml, &ParseLimits::default()).unwrap();
        let kinds: Vec<Kind> = (0..5).map(|xf| formats.kind(xf)).collect();
        assert_eq!(
            kinds,
            [
                Kind::Number,
                Kind::Date,
                Kind::Number,
                Kind::DateTime,
                Kind::Number
            ]
        );
    }

    #[test]
    fn workbook_sheets_use_the_prefixed_relationship_id() {
        let xml = r#"<x:workbook xmlns:x="m" xmlns:r="r"><x:workbookPr date1904="1"/>
<x:sheets><x:sheet name="One" sheetId="5" id="bogus" r:id="rId3"/></x:sheets></x:workbook>"#;
        let wb = parse_workbook(xml, &ParseLimits::default()).unwrap();
        assert!(wb.date1904);
        assert_eq!(
            wb.sheets,
            [("One".to_owned(), "rId3".to_owned(), Visibility::Visible)]
        );
    }
}
