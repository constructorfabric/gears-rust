//! Reader for XLSB (Excel binary) workbooks.
//!
//! An XLSB package is an OOXML ZIP whose workbook, sheets, styles and shared
//! strings are BIFF12 record streams: `[type varint][length varint][body]`.
//! Parts are found through relationships and read through [`Archive`], so the
//! ZIP limits apply. Every record must fit inside its part, and every field is
//! read from the record's own body, so a short record cannot reuse bytes of an
//! earlier one. Formulas contribute their cached result only.

use crate::domain::error::DomainError;
use crate::infra::parsers::limits::{ParseLimits, limit_exceeded};
use crate::infra::parsers::ooxml::{self, Archive};

use super::Sink;
use super::cells::bool_text;
use super::shared::{CellFormats, SharedStrings, error_text, number_text, rk_value};

const BRT_ROW_HDR: u16 = 0x0000;
const BRT_CELL_RK: u16 = 0x0002;
const BRT_CELL_ERROR: u16 = 0x0003;
const BRT_CELL_BOOL: u16 = 0x0004;
const BRT_CELL_REAL: u16 = 0x0005;
const BRT_CELL_ST: u16 = 0x0006;
const BRT_CELL_ISST: u16 = 0x0007;
const BRT_FMLA_STRING: u16 = 0x0008;
const BRT_FMLA_NUM: u16 = 0x0009;
const BRT_FMLA_BOOL: u16 = 0x000A;
const BRT_FMLA_ERROR: u16 = 0x000B;
const BRT_SST_ITEM: u16 = 0x0013;
const BRT_FMT: u16 = 0x002C;
const BRT_XF: u16 = 0x002F;
const BRT_WB_PROP: u16 = 0x0099;
const BRT_BUNDLE_SH: u16 = 0x009C;
const BRT_BEGIN_CELL_XFS: u16 = 0x0269;
const BRT_END_CELL_XFS: u16 = 0x026A;

fn parse_error(detail: impl std::fmt::Display) -> DomainError {
    DomainError::parse_error(format!("Failed to parse XLSB: {detail}"))
}

/// Reads every worksheet of the package into `sink`; returns the title.
pub(super) fn read(
    mut archive: Archive,
    limits: &ParseLimits,
    sink: &mut Sink,
) -> Result<Option<String>, DomainError> {
    let root = archive.read_rels("")?;
    let workbook_part = root
        .of_type("officeDocument")
        .find(|r| !r.external && archive.contains(&r.target))
        .map_or_else(|| "xl/workbook.bin".to_owned(), |r| r.target.clone());
    let title = match root.of_type("core-properties").find(|r| !r.external) {
        Some(rel) => ooxml::core_title(&mut archive, &rel.target.clone())?,
        None => None,
    };

    let workbook = archive
        .read(&workbook_part)?
        .ok_or_else(|| parse_error("no workbook part"))?;
    let (date1904, sheets) = parse_workbook(&workbook, &workbook_part, limits)?;
    drop(workbook);
    let rels = archive.read_rels(&workbook_part)?;

    let shared = match internal(&rels, "sharedStrings") {
        Some(target) => match archive.read(&target)? {
            Some(part) => parse_shared_strings(&part, &target)?,
            None => SharedStrings::default(),
        },
        None => SharedStrings::default(),
    };
    let formats = match internal(&rels, "styles") {
        Some(target) => match archive.read(&target)? {
            Some(part) => parse_styles(&part, &target)?,
            None => CellFormats::default(),
        },
        None => CellFormats::default(),
    };

    for (name, rel_id) in &sheets {
        let Some(rel) = rels
            .get(rel_id)
            .filter(|r| !r.external && r.rel_type.rsplit('/').next() == Some("worksheet"))
        else {
            continue;
        };
        let target = rel.target.clone();
        let Some(part) = archive.read(&target)? else {
            continue;
        };
        let mut cells = sink.new_sheet();
        let mut row: u32 = 0;
        for record in records(&part, &target) {
            let (typ, body) = record?;
            if typ == BRT_ROW_HDR {
                row = u32_at(body, 0).unwrap_or(row);
                continue;
            }
            let Some((col, text)) = cell(typ, body, &shared, &formats, date1904) else {
                continue;
            };
            if !text.is_empty() && !sink.push(&mut cells, row, col, text)? {
                break;
            }
        }
        sink.finish_sheet(name, cells)?;
    }
    Ok(title)
}

fn internal(rels: &crate::infra::parsers::ooxml::Relationships, kind: &str) -> Option<String> {
    rels.of_type(kind)
        .find(|r| !r.external)
        .map(|r| r.target.clone())
}

/// Iterates the records of a binary part: `[type varint][length varint][body]`.
/// A record running past the end of the part ends the iteration with an
/// error, so no length the file declares is ever allocated.
struct Records<'a> {
    data: &'a [u8],
    part: &'a str,
    pos: usize,
}

fn records<'a>(data: &'a [u8], part: &'a str) -> Records<'a> {
    Records { data, part, pos: 0 }
}

impl<'a> Iterator for Records<'a> {
    /// Record type and body.
    type Item = Result<(u16, &'a [u8]), DomainError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.data.len() {
            return None;
        }
        let start = self.pos;
        let item = self.read();
        if item.is_none() {
            // Stop for good after a malformed record.
            self.pos = self.data.len();
            return Some(Err(parse_error(format!(
                "record at offset {start} of part {:?} runs past the part",
                self.part
            ))));
        }
        item.map(Ok)
    }
}

impl<'a> Records<'a> {
    fn read(&mut self) -> Option<(u16, &'a [u8])> {
        // Record type: one or two bytes, seven bits each.
        let mut typ: u16 = 0;
        for i in 0..2 {
            let b = *self.data.get(self.pos)?;
            self.pos += 1;
            typ |= u16::from(b & 0x7F) << (7 * i);
            if b & 0x80 == 0 {
                break;
            }
        }
        // Record length: up to four bytes, seven bits each.
        let mut len: usize = 0;
        for i in 0..4 {
            let b = *self.data.get(self.pos)?;
            self.pos += 1;
            len |= usize::from(b & 0x7F) << (7 * i);
            if b & 0x80 == 0 {
                break;
            }
        }
        let body = self.data.get(self.pos..self.pos.checked_add(len)?)?;
        self.pos += len;
        Some((typ, body))
    }
}

fn u32_at(body: &[u8], at: usize) -> Option<u32> {
    let b = body.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn u16_at(body: &[u8], at: usize) -> Option<u16> {
    let b = body.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

fn f64_at(body: &[u8], at: usize) -> Option<f64> {
    let b: [u8; 8] = body.get(at..at.checked_add(8)?)?.try_into().ok()?;
    Some(f64::from_le_bytes(b))
}

/// An `XLWideString` at `body[at]`: a 32-bit character count and UTF-16LE
/// characters, which must all be present.
fn wide_string(body: &[u8], at: usize) -> Option<String> {
    let cch = usize::try_from(u32_at(body, at)?).ok()?;
    let start = at + 4;
    let bytes = body.get(start..start.checked_add(cch.checked_mul(2)?)?)?;
    let (units, _) = bytes.as_chunks::<2>();
    Some(
        char::decode_utf16(units.iter().map(|&u| u16::from_le_bytes(u)))
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect(),
    )
}

/// (sheet name, relationship id), in workbook order.
type SheetList = Vec<(String, String)>;

/// The 1904 flag and the sheets of the workbook.
fn parse_workbook(
    data: &[u8],
    part: &str,
    limits: &ParseLimits,
) -> Result<(bool, SheetList), DomainError> {
    let mut date1904 = false;
    let mut sheets = Vec::new();
    for record in records(data, part) {
        let (typ, body) = record?;
        match typ {
            BRT_WB_PROP => date1904 = u32_at(body, 0).is_some_and(|f| f & 0x01 != 0),
            BRT_BUNDLE_SH => {
                // state (4), tab id (4), relationship id (nullable), name.
                let Some(id_len) = u32_at(body, 8) else {
                    continue;
                };
                if id_len == u32::MAX {
                    continue;
                }
                let (Some(rel_id), Some(name_at)) = (
                    wide_string(body, 8),
                    usize::try_from(id_len)
                        .ok()
                        .and_then(|n| n.checked_mul(2))
                        .and_then(|n| n.checked_add(12)),
                ) else {
                    continue;
                };
                let name = wide_string(body, name_at).unwrap_or_default();
                sheets.push((name, rel_id));
                if sheets.len() > limits.max_sheets {
                    return Err(limit_exceeded(
                        "max_sheets",
                        format!("more than {} sheets", limits.max_sheets),
                    ));
                }
            }
            _ => {}
        }
    }
    Ok((date1904, sheets))
}

/// `BrtSSTItem` records: option flags, then the string. The declared item
/// count (`BrtBeginSst`) is not used.
fn parse_shared_strings(data: &[u8], part: &str) -> Result<SharedStrings, DomainError> {
    let mut strings = SharedStrings::default();
    for record in records(data, part) {
        let (typ, body) = record?;
        if typ == BRT_SST_ITEM {
            strings.push(&wide_string(body, 1).unwrap_or_default())?;
        }
    }
    Ok(strings)
}

/// Number formats (`BrtFmt`) and the cell formats of the `CellXFs` block.
fn parse_styles(data: &[u8], part: &str) -> Result<CellFormats, DomainError> {
    let mut formats = CellFormats::default();
    let mut in_cell_xfs = false;
    for record in records(data, part) {
        let (typ, body) = record?;
        match typ {
            BRT_FMT => {
                if let (Some(id), Some(code)) = (u16_at(body, 0), wide_string(body, 2)) {
                    formats.add_format(u32::from(id), &code);
                }
            }
            BRT_BEGIN_CELL_XFS => in_cell_xfs = true,
            BRT_END_CELL_XFS => in_cell_xfs = false,
            BRT_XF if in_cell_xfs => {
                // ixfeParent (2), then the number format id (2).
                if let Some(id) = u16_at(body, 2) {
                    formats.add_xf(u32::from(id));
                }
            }
            _ => {}
        }
    }
    Ok(formats)
}

/// The (column, text) of a cell record: column (4), style index in the low
/// 24 bits of the next 4 bytes, then the value.
fn cell(
    typ: u16,
    body: &[u8],
    shared: &SharedStrings,
    formats: &CellFormats,
    date1904: bool,
) -> Option<(u32, String)> {
    let col = u32_at(body, 0)?;
    let xf = usize::try_from(u32_at(body, 4)? & 0x00FF_FFFF).ok()?;
    let number = |value: f64| number_text(value, formats.kind(xf), date1904);
    let text = match typ {
        BRT_CELL_RK => number(rk_value(u32_at(body, 8)?)),
        BRT_CELL_REAL | BRT_FMLA_NUM => number(f64_at(body, 8)?),
        BRT_CELL_BOOL | BRT_FMLA_BOOL => bool_text(*body.get(8)? != 0).to_owned(),
        BRT_CELL_ERROR | BRT_FMLA_ERROR => error_text(*body.get(8)?).to_owned(),
        BRT_CELL_ST | BRT_FMLA_STRING => wide_string(body, 8)?,
        BRT_CELL_ISST => shared
            .get(usize::try_from(u32_at(body, 8)?).ok()?)?
            .to_owned(),
        _ => return None,
    };
    Some((col, text))
}

#[cfg(test)]
pub(super) mod build {
    //! Builds minimal XLSB packages for tests.

    use crate::infra::parsers::ooxml::test_zip;

    pub fn record(typ: u16, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        if typ < 0x80 {
            out.push(u8::try_from(typ).expect("one byte"));
        } else {
            out.push(u8::try_from(typ & 0x7F).expect("low bits") | 0x80);
            out.push(u8::try_from(typ >> 7).expect("high bits"));
        }
        let mut len = body.len();
        loop {
            let byte = u8::try_from(len & 0x7F).expect("seven bits");
            len >>= 7;
            if len == 0 {
                out.push(byte);
                break;
            }
            out.push(byte | 0x80);
        }
        out.extend_from_slice(body);
        out
    }

    pub fn wide(s: &str) -> Vec<u8> {
        let units: Vec<u16> = s.encode_utf16().collect();
        let mut out = u32::try_from(units.len())
            .expect("short")
            .to_le_bytes()
            .to_vec();
        out.extend(units.iter().flat_map(|u| u.to_le_bytes()));
        out
    }

    /// `workbook.bin` listing (relationship id, sheet name) pairs.
    pub fn workbook(sheets: &[(&str, &str)], date1904: bool) -> Vec<u8> {
        let mut out = record(super::BRT_WB_PROP, &u32::from(date1904).to_le_bytes());
        for (id, name) in sheets {
            let mut body = vec![0, 0, 0, 0, 1, 0, 0, 0];
            body.extend(wide(id));
            body.extend(wide(name));
            out.extend(record(super::BRT_BUNDLE_SH, &body));
        }
        out
    }

    fn cell_head(col: u32, xf: u32) -> Vec<u8> {
        let mut body = col.to_le_bytes().to_vec();
        body.extend_from_slice(&xf.to_le_bytes());
        body
    }

    pub fn row(n: u32) -> Vec<u8> {
        let mut body = n.to_le_bytes().to_vec();
        body.extend_from_slice(&[0; 13]);
        record(super::BRT_ROW_HDR, &body)
    }

    pub fn real(col: u32, xf: u32, value: f64) -> Vec<u8> {
        let mut body = cell_head(col, xf);
        body.extend_from_slice(&value.to_le_bytes());
        record(super::BRT_CELL_REAL, &body)
    }

    pub fn isst(col: u32, index: u32) -> Vec<u8> {
        let mut body = cell_head(col, 0);
        body.extend_from_slice(&index.to_le_bytes());
        record(super::BRT_CELL_ISST, &body)
    }

    pub fn text(col: u32, s: &str) -> Vec<u8> {
        let mut body = cell_head(col, 0);
        body.extend(wide(s));
        record(super::BRT_CELL_ST, &body)
    }

    pub fn boolean(col: u32, value: bool) -> Vec<u8> {
        let mut body = cell_head(col, 0);
        body.push(u8::from(value));
        record(super::BRT_CELL_BOOL, &body)
    }

    pub fn sst(strings: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for s in strings {
            let mut body = vec![0];
            body.extend(wide(s));
            out.extend(record(super::BRT_SST_ITEM, &body));
        }
        out
    }

    pub fn styles(formats: &[(u16, &str)], xf_formats: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        for (id, code) in formats {
            let mut body = id.to_le_bytes().to_vec();
            body.extend(wide(code));
            out.extend(record(super::BRT_FMT, &body));
        }
        out.extend(record(super::BRT_BEGIN_CELL_XFS, &[0; 4]));
        for id in xf_formats {
            let mut body = 0_u16.to_le_bytes().to_vec();
            body.extend_from_slice(&id.to_le_bytes());
            body.extend_from_slice(&[0; 12]);
            out.extend(record(super::BRT_XF, &body));
        }
        out.extend(record(super::BRT_END_CELL_XFS, &[]));
        out
    }

    /// A package: `workbook`, sheets `(id, target, data)`, optional shared
    /// strings and styles parts.
    pub fn package(
        workbook: &[u8],
        sheets: &[(&str, &str, Vec<u8>)],
        sst: Option<Vec<u8>>,
        styles: Option<Vec<u8>>,
    ) -> bytes::Bytes {
        let rel = |id: &str, kind: &str, target: &str| {
            format!(
                r#"<Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/{kind}" Target="{target}"/>"#
            )
        };
        let mut rels = String::new();
        let mut parts: Vec<(String, Vec<u8>)> =
            vec![("xl/workbook.bin".to_owned(), workbook.to_vec())];
        for (id, target, data) in sheets {
            rels.push_str(&rel(id, "worksheet", target));
            parts.push((format!("xl/{target}"), data.clone()));
        }
        if let Some(sst) = sst {
            rels.push_str(&rel("rIdS", "sharedStrings", "sharedStrings.bin"));
            parts.push(("xl/sharedStrings.bin".to_owned(), sst));
        }
        if let Some(styles) = styles {
            rels.push_str(&rel("rIdT", "styles", "styles.bin"));
            parts.push(("xl/styles.bin".to_owned(), styles));
        }
        parts.push((
            "xl/_rels/workbook.bin.rels".to_owned(),
            format!(r#"<Relationships xmlns="r">{rels}</Relationships>"#).into_bytes(),
        ));
        parts.push((
            "_rels/.rels".to_owned(),
            br#"<Relationships xmlns="r"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.bin"/></Relationships>"#.to_vec(),
        ));
        let entries: Vec<(&str, &[u8])> = parts
            .iter()
            .map(|(n, d)| (n.as_str(), d.as_slice()))
            .collect();
        test_zip::build(&entries)
    }
}

#[cfg(test)]
mod tests {
    use super::build::*;
    use super::*;

    fn types(data: &[u8]) -> Result<Vec<u16>, DomainError> {
        records(data, "p").map(|r| r.map(|(typ, _)| typ)).collect()
    }

    #[test]
    fn record_framing() {
        // type 0x01 len 2; two-byte type 0x0101 (0x81 0x02) len 0.
        assert_eq!(
            types(&[0x01, 0x02, 0xAA, 0xBB, 0x81, 0x02, 0x00]).unwrap(),
            [0x01, 0x0101]
        );
        assert!(types(&[]).unwrap().is_empty());
        // Length 0x0FFF_FFFF in a seven-byte part, a truncated header.
        assert!(types(&[0x01, 0xFF, 0xFF, 0xFF, 0x7F, 0x00, 0x00]).is_err());
        assert!(types(&[0x01, 0x80]).is_err());
    }

    #[test]
    fn short_records_never_borrow_bytes_from_earlier_ones() {
        // An empty BrtSSTItem after a long one is an empty string, not a copy.
        let mut data = sst(&["a long shared string"]);
        data.extend(record(BRT_SST_ITEM, &[]));
        let strings = parse_shared_strings(&data, "s").unwrap();
        assert_eq!(strings.to_vec(), ["a long shared string", ""]);
        // A cell record too short for its value is skipped.
        assert_eq!(
            cell(
                BRT_CELL_REAL,
                &[0; 10],
                &strings,
                &CellFormats::default(),
                false
            ),
            None
        );
    }

    #[test]
    fn wide_strings_must_be_complete() {
        let mut body = wide("abc");
        assert_eq!(wide_string(&body, 0).as_deref(), Some("abc"));
        body.truncate(body.len() - 1);
        assert_eq!(wide_string(&body, 0), None);
        let mut huge = u32::MAX.to_le_bytes().to_vec();
        huge.extend_from_slice(b"xy");
        assert_eq!(wide_string(&huge, 0), None);
    }

    #[test]
    fn styles_and_dates() {
        let formats = parse_styles(&styles(&[(164, "yyyy-mm-dd")], &[0, 164]), "s").unwrap();
        let empty = SharedStrings::default();
        let date = real(0, 1, 45_352.0);
        let body = &date[2..];
        assert_eq!(
            cell(BRT_CELL_REAL, body, &empty, &formats, false),
            Some((0, "2024-03-01".to_owned()))
        );
    }
}
