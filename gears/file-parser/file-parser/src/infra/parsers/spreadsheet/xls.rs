//! Reader for legacy `.xls` workbooks (BIFF8, and BIFF5 text).
//!
//! The workbook is the `Workbook` (or `Book`) stream of an OLE compound file:
//! a sequence of `[type u16][length u16][body]` records. The globals come
//! first (formats, cell formats, shared strings, the sheet list), then one
//! substream per sheet. Only cell values are read; formulas contribute their
//! cached result and their tokens are never decoded. Nothing is sized from a
//! count or dimension the file declares: cells go into the sparse
//! [`SheetCells`](super::SheetCells) under the document budgets, and each sheet
//! walk stops at the start of the next sheet, so the stream is walked once.

use std::io::{Cursor, Read};

use crate::domain::error::DomainError;
use crate::infra::parsers::limits::{ParseLimits, limit_exceeded};

use super::cells::bool_text;
use super::shared::{CellFormats, SharedStrings, error_text, number_text, rk_value};
use super::{SheetCells, Sink};

const BOF: u16 = 0x0809;
const EOF: u16 = 0x000A;
const FILEPASS: u16 = 0x002F;
const CODEPAGE: u16 = 0x0042;
const DATEMODE: u16 = 0x0022;
const FORMAT: u16 = 0x041E;
const XF: u16 = 0x00E0;
const SST: u16 = 0x00FC;
const CONTINUE: u16 = 0x003C;
const BOUNDSHEET: u16 = 0x0085;
const NUMBER: u16 = 0x0203;
const RK: u16 = 0x027E;
const MULRK: u16 = 0x00BD;
const LABEL_SST: u16 = 0x00FD;
const LABEL: u16 = 0x0204;
const RSTRING: u16 = 0x00D6;
const BOOLERR: u16 = 0x0205;
const FORMULA: u16 = 0x0006;
/// Cached string result of the preceding formula.
const FORMULA_STRING: u16 = 0x0207;

const BIFF8: u16 = 0x0600;
const BIFF5: u16 = 0x0500;

fn parse_error(detail: impl std::fmt::Display) -> DomainError {
    DomainError::parse_error(format!("Failed to parse XLS: {detail}"))
}

/// Reads every worksheet of the compound file `bytes` into `sink`.
pub(super) fn read(bytes: &[u8], limits: &ParseLimits, sink: &mut Sink) -> Result<(), DomainError> {
    let stream = workbook_stream(bytes, limits)?;
    let globals = read_globals(&stream, limits)?;
    let bounds = sheet_bounds(&globals.sheets, stream.len())?;
    for (sheet, end) in globals.sheets.iter().zip(bounds) {
        if !sheet.worksheet {
            continue;
        }
        let mut cells = sink.new_sheet();
        read_sheet(&stream, sheet.offset, end, &globals, sink, &mut cells)?;
        sink.finish_sheet(&sheet.name, cells)?;
    }
    Ok(())
}

/// The BIFF stream: `Workbook` (BIFF8) or `Book` (BIFF5), at the root.
fn workbook_stream(bytes: &[u8], limits: &ParseLimits) -> Result<Vec<u8>, DomainError> {
    let mut file = cfb::CompoundFile::open(Cursor::new(bytes)).map_err(parse_error)?;
    let name = ["/Workbook", "/Book"]
        .into_iter()
        .find(|name| file.is_stream(name))
        .ok_or_else(|| parse_error("no Workbook stream"))?;
    let stream = file.open_stream(name).map_err(parse_error)?;
    let mut data = Vec::new();
    stream
        .take(limits.max_entry_bytes.saturating_add(1))
        .read_to_end(&mut data)
        .map_err(parse_error)?;
    if data.len() as u64 > limits.max_entry_bytes {
        return Err(limit_exceeded(
            "max_entry_bytes",
            format!("Workbook stream exceeds {} bytes", limits.max_entry_bytes),
        ));
    }
    Ok(data)
}

/// One record: type and body, plus the offset of the next record.
fn record_at(data: &[u8], pos: usize) -> Option<(u16, &[u8], usize)> {
    let header = data.get(pos..pos.checked_add(4)?)?;
    let typ = u16::from_le_bytes([header[0], header[1]]);
    let len = usize::from(u16::from_le_bytes([header[2], header[3]]));
    let start = pos + 4;
    let body = data.get(start..start.checked_add(len)?)?;
    Some((typ, body, start + len))
}

fn u16_at(body: &[u8], at: usize) -> Option<u16> {
    let b = body.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(body: &[u8], at: usize) -> Option<u32> {
    let b = body.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn f64_at(body: &[u8], at: usize) -> Option<f64> {
    let b: [u8; 8] = body.get(at..at.checked_add(8)?)?.try_into().ok()?;
    Some(f64::from_le_bytes(b))
}

/// Latin-1: each byte is the code point of the same value, which is what a
/// BIFF8 "compressed" (8-bit) string is.
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

/// How the workbook stores text.
#[derive(Debug, Clone, Copy)]
enum Text {
    /// BIFF8: option flags per string, 8-bit Latin-1 or UTF-16.
    Biff8,
    /// BIFF5: bytes in the workbook's code page.
    Biff5(&'static encoding_rs::Encoding),
}

/// The encoding of a Windows code page number (CODEPAGE record). Unknown or
/// DOS code pages fall back to Windows-1252, Excel's default.
fn code_page(number: u16) -> &'static encoding_rs::Encoding {
    use encoding_rs as e;
    match number {
        866 => e::IBM866,
        874 => e::WINDOWS_874,
        932 => e::SHIFT_JIS,
        936 => e::GBK,
        949 => e::EUC_KR,
        950 => e::BIG5,
        1250 => e::WINDOWS_1250,
        1251 => e::WINDOWS_1251,
        1253 => e::WINDOWS_1253,
        1254 => e::WINDOWS_1254,
        1255 => e::WINDOWS_1255,
        1256 => e::WINDOWS_1256,
        1257 => e::WINDOWS_1257,
        1258 => e::WINDOWS_1258,
        10000 => e::MACINTOSH,
        10007 => e::X_MAC_CYRILLIC,
        _ => e::WINDOWS_1252,
    }
}

fn utf16(bytes: &[u8]) -> String {
    let (units, _) = bytes.as_chunks::<2>();
    char::decode_utf16(units.iter().map(|&u| u16::from_le_bytes(u)))
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// A string inside one record, starting at `body[at]`: BIFF8 `XLUnicodeString`
/// (length, option flags, optional run/extension counts, characters) or a
/// BIFF5 byte string. `len_bytes` is the width of the length field (1 or 2).
/// A string cut short by the record end yields the characters present.
fn record_string(body: &[u8], at: usize, len_bytes: usize, text: Text) -> Option<String> {
    let cch = match len_bytes {
        1 => usize::from(*body.get(at)?),
        _ => usize::from(u16_at(body, at)?),
    };
    let mut pos = at + len_bytes;
    if let Text::Biff5(encoding) = text {
        let end = (pos + cch).min(body.len());
        let (decoded, _, _) = encoding.decode(body.get(pos..end)?);
        return Some(decoded.into_owned());
    }
    let flags = *body.get(pos)?;
    pos += 1;
    if flags & 0x08 != 0 {
        pos += 2; // formatting run count
    }
    if flags & 0x04 != 0 {
        pos += 4; // phonetic extension size
    }
    if flags & 0x01 != 0 {
        let end = (pos + cch * 2).min(body.len());
        Some(utf16(body.get(pos..end)?))
    } else {
        let end = (pos + cch).min(body.len());
        Some(latin1(body.get(pos..end)?))
    }
}

struct SheetEntry {
    offset: usize,
    name: String,
    worksheet: bool,
}

struct Globals {
    text: Text,
    date1904: bool,
    formats: CellFormats,
    sst: SharedStrings,
    sheets: Vec<SheetEntry>,
}

fn read_globals(data: &[u8], limits: &ParseLimits) -> Result<Globals, DomainError> {
    let (typ, body, mut pos) = record_at(data, 0).ok_or_else(|| parse_error("empty stream"))?;
    let version = u16_at(body, 0).unwrap_or(0);
    if typ != BOF || !(version == BIFF8 || version == BIFF5) {
        return Err(parse_error(
            "not a BIFF5 or BIFF8 workbook (older Excel versions are not supported)",
        ));
    }
    let biff8 = version == BIFF8;
    let mut globals = Globals {
        text: if biff8 {
            Text::Biff8
        } else {
            Text::Biff5(encoding_rs::WINDOWS_1252)
        },
        date1904: false,
        formats: CellFormats::default(),
        sst: SharedStrings::default(),
        sheets: Vec::new(),
    };
    while let Some((typ, body, next)) = record_at(data, pos) {
        match typ {
            FILEPASS => return Err(parse_error("encrypted workbooks are not supported")),
            DATEMODE => globals.date1904 = u16_at(body, 0).is_some_and(|v| v != 0),
            CODEPAGE if !biff8 => {
                if let Some(number) = u16_at(body, 0) {
                    globals.text = Text::Biff5(code_page(number));
                }
            }
            FORMAT => {
                let len_bytes = if biff8 { 2 } else { 1 };
                if let (Some(id), Some(code)) = (
                    u16_at(body, 0),
                    record_string(body, 2, len_bytes, globals.text),
                ) {
                    globals.formats.add_format(u32::from(id), &code);
                }
            }
            XF => {
                if let Some(id) = u16_at(body, 2) {
                    globals.formats.add_xf(u32::from(id));
                }
            }
            SST if biff8 => globals.sst = read_sst(data, pos)?,
            BOUNDSHEET => {
                let (Some(offset), Some(&kind)) = (u32_at(body, 0), body.get(5)) else {
                    return Err(parse_error("invalid BOUNDSHEET record"));
                };
                let name = record_string(body, 6, 1, globals.text).unwrap_or_default();
                globals.sheets.push(SheetEntry {
                    offset: usize::try_from(offset).unwrap_or(usize::MAX),
                    name,
                    worksheet: kind == 0,
                });
                if globals.sheets.len() > limits.max_sheets {
                    return Err(limit_exceeded(
                        "max_sheets",
                        format!("more than {} sheets", limits.max_sheets),
                    ));
                }
            }
            EOF => break,
            _ => {}
        }
        pos = next;
    }
    Ok(globals)
}

/// End offset of each sheet's substream: the next sheet's start (or the end
/// of the stream). Sheets sharing a start are rejected, so the walks together
/// cover the stream at most once.
fn sheet_bounds(sheets: &[SheetEntry], len: usize) -> Result<Vec<usize>, DomainError> {
    let mut starts: Vec<usize> = sheets.iter().map(|s| s.offset).collect();
    starts.sort_unstable();
    if starts.windows(2).any(|w| w[0] == w[1]) {
        return Err(parse_error("two sheets share one substream"));
    }
    Ok(sheets
        .iter()
        .map(|s| {
            let next = starts.partition_point(|&o| o <= s.offset);
            starts.get(next).copied().unwrap_or(len).min(len)
        })
        .collect())
}

/// The shared string table: the SST record at `pos` and the CONTINUE records
/// after it. Strings are read until the data runs out; the declared counts are
/// not trusted.
fn read_sst(data: &[u8], pos: usize) -> Result<SharedStrings, DomainError> {
    let mut strings = SharedStrings::default();
    let mut cursor = SstCursor::new(data, pos);
    if cursor.skip(8).is_none() {
        return Ok(strings);
    }
    let mut units: Vec<u16> = Vec::new();
    while cursor.string(&mut units).is_some() {
        let text: String = char::decode_utf16(units.iter().copied())
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
        strings.push(&text)?;
    }
    Ok(strings)
}

/// Reads across an SST record and the CONTINUE records after it.
struct SstCursor<'a> {
    data: &'a [u8],
    body: &'a [u8],
    next: usize,
}

impl<'a> SstCursor<'a> {
    fn new(data: &'a [u8], pos: usize) -> Self {
        let (body, next) = record_at(data, pos).map_or((&[][..], data.len()), |(_, b, n)| (b, n));
        Self { data, body, next }
    }

    /// Moves to the next CONTINUE record; `false` at the end of the table.
    fn advance(&mut self) -> bool {
        match record_at(self.data, self.next) {
            Some((CONTINUE, body, next)) => {
                self.body = body;
                self.next = next;
                true
            }
            _ => false,
        }
    }

    /// The next `N` bytes, read across record boundaries.
    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        let mut out = [0_u8; N];
        let mut filled = 0;
        while filled < N {
            if self.body.is_empty() && !self.advance() {
                return None;
            }
            let take = (N - filled).min(self.body.len());
            out[filled..filled + take].copy_from_slice(&self.body[..take]);
            self.body = &self.body[take..];
            filled += take;
        }
        Some(out)
    }

    fn skip(&mut self, mut n: usize) -> Option<()> {
        while n > 0 {
            if self.body.is_empty() && !self.advance() {
                return None;
            }
            let take = n.min(self.body.len());
            self.body = &self.body[take..];
            n -= take;
        }
        Some(())
    }

    /// One `XLUnicodeRichExtendedString` into `units` (replacing its content).
    /// Character data split across a CONTINUE record restarts with a fresh
    /// option byte saying whether the rest is 8- or 16-bit.
    fn string(&mut self, units: &mut Vec<u16>) -> Option<()> {
        units.clear();
        let cch = usize::from(u16::from_le_bytes(self.array::<2>()?));
        let [flags] = self.array::<1>()?;
        let runs = if flags & 0x08 != 0 {
            usize::from(u16::from_le_bytes(self.array::<2>()?))
        } else {
            0
        };
        let ext = if flags & 0x04 != 0 {
            usize::try_from(u32::from_le_bytes(self.array::<4>()?)).ok()?
        } else {
            0
        };
        let mut wide = flags & 0x01 != 0;
        while units.len() < cch {
            if self.body.is_empty() {
                if !self.advance() {
                    return None;
                }
                let [option] = self.array::<1>()?;
                wide = option & 0x01 != 0;
                continue;
            }
            let left = cch - units.len();
            if wide {
                let (pairs, _) = self.body.as_chunks::<2>();
                let take = pairs.len().min(left);
                if take == 0 {
                    // A dangling byte cannot hold a character.
                    return None;
                }
                units.extend(pairs[..take].iter().map(|&p| u16::from_le_bytes(p)));
                self.body = &self.body[take * 2..];
            } else {
                let take = self.body.len().min(left);
                units.extend(self.body[..take].iter().map(|&b| u16::from(b)));
                self.body = &self.body[take..];
            }
        }
        self.skip(runs * 4)?;
        self.skip(ext)?;
        Some(())
    }
}

fn read_sheet(
    data: &[u8],
    start: usize,
    end: usize,
    globals: &Globals,
    sink: &mut Sink,
    cells: &mut SheetCells,
) -> Result<(), DomainError> {
    let mut seen_bof = false;
    let mut nested = 0_usize;
    // Position of the last formula whose string result follows.
    let mut pending_string: Option<(u32, u32)> = None;
    let mut pos = start;
    while let Some((typ, body, next)) = record_at(data, pos).filter(|&(_, _, n)| n <= end) {
        let record_pos = pos;
        pos = next;
        // Embedded chart substreams hold no cells of this sheet.
        if nested > 0 {
            match typ {
                BOF => nested += 1,
                EOF => nested -= 1,
                _ => {}
            }
            continue;
        }
        match typ {
            BOF if seen_bof => nested += 1,
            BOF => seen_bof = true,
            EOF => break,
            MULRK => {
                for (row, col, text) in mulrk_cells(body, globals) {
                    if !sink.push(cells, row, col, text)? {
                        return Ok(());
                    }
                }
            }
            _ => {
                if let Some((row, col, text)) =
                    cell(typ, body, data, record_pos, globals, &mut pending_string)
                    && !text.is_empty()
                    && !sink.push(cells, row, col, text)?
                {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

/// Text of a number shown through cell format `xf`.
fn number(globals: &Globals, xf: Option<u16>, value: f64) -> String {
    let kind = globals.formats.kind(xf.map_or(0, usize::from));
    number_text(value, kind, globals.date1904)
}

/// The cells of a MULRK record: row, first column, `(xf u16, rk u32)` per
/// cell, last column.
fn mulrk_cells<'a>(
    body: &'a [u8],
    globals: &'a Globals,
) -> impl Iterator<Item = (u32, u32, String)> + 'a {
    let row = u16_at(body, 0).map(u32::from);
    let first = u16_at(body, 2).map(u32::from);
    let values = body.get(4..body.len().saturating_sub(2)).unwrap_or(&[]);
    let (pairs, _) = values.as_chunks::<6>();
    pairs.iter().enumerate().filter_map(move |(i, pair)| {
        let col = first?.checked_add(u32::try_from(i).ok()?)?;
        let xf = Some(u16::from_le_bytes([pair[0], pair[1]]));
        let rk = u32::from_le_bytes([pair[2], pair[3], pair[4], pair[5]]);
        Some((row?, col, number(globals, xf, rk_value(rk))))
    })
}

/// The cell a single-cell record holds, if any.
fn cell(
    typ: u16,
    body: &[u8],
    data: &[u8],
    record_pos: usize,
    globals: &Globals,
    pending_string: &mut Option<(u32, u32)>,
) -> Option<(u32, u32, String)> {
    if typ == FORMULA_STRING {
        let (row, col) = pending_string.take()?;
        let text = match globals.text {
            // A long result continues in CONTINUE records, like SST strings.
            Text::Biff8 => {
                let mut units = Vec::new();
                SstCursor::new(data, record_pos).string(&mut units)?;
                char::decode_utf16(units)
                    .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
                    .collect()
            }
            Text::Biff5(_) => record_string(body, 0, 2, globals.text)?,
        };
        return Some((row, col, text));
    }
    let row = u32::from(u16_at(body, 0)?);
    let col = u32::from(u16_at(body, 2)?);
    let xf = u16_at(body, 4);
    let text = match typ {
        NUMBER => number(globals, xf, f64_at(body, 6)?),
        RK => number(globals, xf, rk_value(u32_at(body, 6)?)),
        LABEL_SST => globals
            .sst
            .get(usize::try_from(u32_at(body, 6)?).ok()?)?
            .to_owned(),
        LABEL | RSTRING => record_string(body, 6, 2, globals.text)?,
        BOOLERR => {
            let (value, is_error) = (*body.get(6)?, *body.get(7)?);
            if is_error != 0 {
                error_text(value).to_owned()
            } else {
                bool_text(value != 0).to_owned()
            }
        }
        FORMULA => {
            // The cached result; the tokens after it are never decoded.
            let result: [u8; 8] = body.get(6..14)?.try_into().ok()?;
            *pending_string = None;
            // 0xFFFF in the top bytes marks a non-numeric result.
            if result[6..8] == [0xFF, 0xFF] {
                match result[0] {
                    0 => {
                        *pending_string = Some((row, col));
                        return None;
                    }
                    1 => bool_text(result[2] != 0).to_owned(),
                    2 => error_text(result[2]).to_owned(),
                    _ => return None,
                }
            } else {
                number(globals, xf, f64::from_le_bytes(result))
            }
        }
        _ => return None,
    };
    Some((row, col, text))
}

#[cfg(test)]
pub(super) mod biff {
    //! Builds minimal BIFF8 workbooks for tests.

    use std::io::{Cursor, Write};

    pub fn record(typ: u16, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + body.len());
        out.extend_from_slice(&typ.to_le_bytes());
        out.extend_from_slice(
            &u16::try_from(body.len())
                .expect("short record")
                .to_le_bytes(),
        );
        out.extend_from_slice(body);
        out
    }

    pub fn bof(sheet: bool) -> Vec<u8> {
        let dt: u16 = if sheet { 0x0010 } else { 0x0005 };
        let mut body = super::BIFF8.to_le_bytes().to_vec();
        body.extend_from_slice(&dt.to_le_bytes());
        body.extend_from_slice(&[0; 12]);
        record(super::BOF, &body)
    }

    pub fn eof() -> Vec<u8> {
        record(super::EOF, &[])
    }

    fn head(row: u16, col: u16, xf: u16) -> Vec<u8> {
        let mut body = row.to_le_bytes().to_vec();
        body.extend_from_slice(&col.to_le_bytes());
        body.extend_from_slice(&xf.to_le_bytes());
        body
    }

    pub fn number(row: u16, col: u16, value: f64) -> Vec<u8> {
        number_xf(row, col, 0, value)
    }

    pub fn number_xf(row: u16, col: u16, xf: u16, value: f64) -> Vec<u8> {
        let mut body = head(row, col, xf);
        body.extend_from_slice(&value.to_le_bytes());
        record(super::NUMBER, &body)
    }

    pub fn rk(row: u16, col: u16, rk: u32) -> Vec<u8> {
        let mut body = head(row, col, 0);
        body.extend_from_slice(&rk.to_le_bytes());
        record(super::RK, &body)
    }

    pub fn mulrk(row: u16, first: u16, values: &[u32]) -> Vec<u8> {
        let mut body = row.to_le_bytes().to_vec();
        body.extend_from_slice(&first.to_le_bytes());
        for rk in values {
            body.extend_from_slice(&0_u16.to_le_bytes());
            body.extend_from_slice(&rk.to_le_bytes());
        }
        let last = first + u16::try_from(values.len()).expect("few") - 1;
        body.extend_from_slice(&last.to_le_bytes());
        record(super::MULRK, &body)
    }

    pub fn label_sst(row: u16, col: u16, index: u32) -> Vec<u8> {
        let mut body = head(row, col, 0);
        body.extend_from_slice(&index.to_le_bytes());
        record(super::LABEL_SST, &body)
    }

    /// A LABEL with a 16-bit string.
    pub fn label(row: u16, col: u16, text: &str) -> Vec<u8> {
        let mut body = head(row, col, 0);
        let units: Vec<u16> = text.encode_utf16().collect();
        body.extend_from_slice(&u16::try_from(units.len()).expect("short").to_le_bytes());
        body.push(0x01);
        body.extend(units.iter().flat_map(|u| u.to_le_bytes()));
        record(super::LABEL, &body)
    }

    pub fn boolerr(row: u16, col: u16, value: u8, is_error: bool) -> Vec<u8> {
        let mut body = head(row, col, 0);
        body.push(value);
        body.push(u8::from(is_error));
        record(super::BOOLERR, &body)
    }

    /// A formula whose cached result is `result` (8 bytes) with `tokens`
    /// bytes of formula tokens.
    pub fn formula(row: u16, col: u16, result: [u8; 8], tokens: u16) -> Vec<u8> {
        let mut body = head(row, col, 0);
        body.extend_from_slice(&result);
        body.extend_from_slice(&[0; 2]); // grbit
        body.extend_from_slice(&[0; 4]); // chn
        body.extend_from_slice(&tokens.to_le_bytes());
        body.extend(std::iter::repeat_n(0x16_u8, usize::from(tokens)));
        record(super::FORMULA, &body)
    }

    pub fn formula_string(text: &str) -> Vec<u8> {
        let mut body = u16::try_from(text.len())
            .expect("short")
            .to_le_bytes()
            .to_vec();
        body.push(0);
        body.extend_from_slice(text.as_bytes());
        record(super::FORMULA_STRING, &body)
    }

    /// An SST record holding 8-bit `strings`.
    pub fn sst(strings: &[&str]) -> Vec<u8> {
        let count = u32::try_from(strings.len()).expect("few");
        let mut body = count.to_le_bytes().to_vec();
        body.extend_from_slice(&count.to_le_bytes());
        for s in strings {
            body.extend_from_slice(&u16::try_from(s.len()).expect("short").to_le_bytes());
            body.push(0);
            body.extend_from_slice(s.as_bytes());
        }
        record(super::SST, &body)
    }

    pub fn format(id: u16, code: &str) -> Vec<u8> {
        let mut body = id.to_le_bytes().to_vec();
        body.extend_from_slice(&u16::try_from(code.len()).expect("short").to_le_bytes());
        body.push(0);
        body.extend_from_slice(code.as_bytes());
        record(super::FORMAT, &body)
    }

    pub fn xf(format_id: u16) -> Vec<u8> {
        let mut body = 0_u16.to_le_bytes().to_vec();
        body.extend_from_slice(&format_id.to_le_bytes());
        body.extend_from_slice(&[0; 16]);
        record(super::XF, &body)
    }

    /// A workbook stream: `globals` after the globals BOF, then one sheet per
    /// `(name, records)`.
    pub fn workbook_with(globals: &[Vec<u8>], sheets: &[(&str, Vec<Vec<u8>>)]) -> Vec<u8> {
        let boundsheet = |offset: u32, name: &str| {
            let mut body = offset.to_le_bytes().to_vec();
            body.extend_from_slice(&[0, 0]); // visible worksheet
            body.push(u8::try_from(name.len()).expect("short"));
            body.push(0);
            body.extend_from_slice(name.as_bytes());
            record(super::BOUNDSHEET, &body)
        };
        let globals_len = bof(false).len()
            + globals.iter().map(Vec::len).sum::<usize>()
            + sheets
                .iter()
                .map(|(n, _)| boundsheet(0, n).len())
                .sum::<usize>()
            + eof().len();
        let mut substreams = Vec::new();
        let mut offsets = Vec::new();
        for (_, records) in sheets {
            offsets.push(u32::try_from(globals_len + substreams.len()).expect("small"));
            substreams.extend(bof(true));
            for r in records {
                substreams.extend_from_slice(r);
            }
            substreams.extend(eof());
        }
        let mut out = bof(false);
        for r in globals {
            out.extend_from_slice(r);
        }
        for ((name, _), offset) in sheets.iter().zip(offsets) {
            out.extend(boundsheet(offset, name));
        }
        out.extend(eof());
        assert_eq!(out.len(), globals_len);
        out.extend(substreams);
        out
    }

    pub fn workbook(name: &str, records: &[Vec<u8>]) -> Vec<u8> {
        workbook_with(&[], &[(name, records.to_vec())])
    }

    /// `stream` wrapped in an OLE compound file under `path`.
    pub fn xls_at(path: &str, stream: &[u8]) -> Vec<u8> {
        let mut file =
            cfb::CompoundFile::create_with_version(cfb::Version::V3, Cursor::new(Vec::new()))
                .expect("create");
        if let Some((dir, _)) = path.rsplit_once('/')
            && !dir.is_empty()
        {
            file.create_storage_all(dir).expect("storage");
        }
        {
            let mut s = file.create_stream(path).expect("stream");
            s.write_all(stream).expect("write");
        }
        file.flush().expect("flush");
        file.into_inner().into_inner()
    }

    pub fn xls(stream: &[u8]) -> Vec<u8> {
        xls_at("/Workbook", stream)
    }
}

#[cfg(test)]
mod tests {
    use super::biff::*;
    use super::*;

    fn rk_int(n: i32) -> u32 {
        u32::from_le_bytes((n << 2).to_le_bytes()) | 0x02
    }

    #[test]
    fn sst_strings_split_across_continue_records() {
        // A 16-bit string of 10 units split after 3, the rest 8-bit, then a
        // second string.
        let mut sst_body = 2_u32.to_le_bytes().to_vec();
        sst_body.extend_from_slice(&2_u32.to_le_bytes());
        sst_body.extend_from_slice(&10_u16.to_le_bytes());
        sst_body.push(0x01);
        sst_body.extend_from_slice(&[b'a', 0, b'b', 0, 0xE9, 0]);
        let mut cont = vec![0x00];
        cont.extend_from_slice(b"defghij");
        cont.extend_from_slice(&2_u16.to_le_bytes());
        cont.push(0x00);
        cont.extend_from_slice(b"xy");
        let mut stream = record(SST, &sst_body);
        stream.extend(record(CONTINUE, &cont));
        let sst = read_sst(&stream, 0).unwrap();
        assert_eq!(sst.to_vec(), ["ab\u{e9}defghij", "xy"]);
    }

    #[test]
    fn declared_sst_count_is_ignored() {
        // cstUnique = 0 but two strings present; and the reverse: a huge count
        // with no strings costs nothing.
        let mut body = 0_u32.to_le_bytes().to_vec();
        body.extend_from_slice(&0_u32.to_le_bytes());
        body.extend_from_slice(&[1, 0, 0, b'a', 1, 0, 0, b'b']);
        assert_eq!(
            read_sst(&record(SST, &body), 0).unwrap().to_vec(),
            ["a", "b"]
        );
        let mut huge = u32::MAX.to_le_bytes().to_vec();
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(
            read_sst(&record(SST, &huge), 0)
                .unwrap()
                .to_vec()
                .is_empty()
        );
    }

    #[test]
    fn many_empty_continue_records_end_the_table() {
        // Each empty CONTINUE is passed once; a quadratic walk would not
        // finish in test time.
        let mut stream = sst(&[]);
        for _ in 0..200_000 {
            stream.extend(record(CONTINUE, &[]));
        }
        assert!(read_sst(&stream, 0).unwrap().to_vec().is_empty());
    }

    #[test]
    fn strings_in_records() {
        // 8-bit, 16-bit, rich with runs, cut short.
        let mut body = vec![0; 6];
        body.extend_from_slice(&[3, 0, 0x00, b'a', b'b', b'c']);
        assert_eq!(
            record_string(&body, 6, 2, Text::Biff8).as_deref(),
            Some("abc")
        );
        let mut wide = vec![0; 6];
        wide.extend_from_slice(&[1, 0, 0x01, 0x3B, 0x04]);
        assert_eq!(
            record_string(&wide, 6, 2, Text::Biff8).as_deref(),
            Some("\u{43b}")
        );
        let mut rich = vec![0; 6];
        rich.extend_from_slice(&[2, 0, 0x08, 1, 0, b'h', b'i']);
        assert_eq!(
            record_string(&rich, 6, 2, Text::Biff8).as_deref(),
            Some("hi")
        );
        let mut short = vec![0; 6];
        short.extend_from_slice(&[9, 0, 0x00, b'x']);
        assert_eq!(
            record_string(&short, 6, 2, Text::Biff8).as_deref(),
            Some("x")
        );
        let biff5 = [2, 0, b'o', 0x80];
        assert_eq!(
            record_string(&biff5, 0, 2, Text::Biff5(encoding_rs::WINDOWS_1252)).as_deref(),
            Some("o\u{20ac}"),
            "0x80 is the euro sign in Windows-1252"
        );
        let cyrillic = [1, 0, 0xE4];
        assert_eq!(
            record_string(&cyrillic, 0, 2, Text::Biff5(code_page(1251))).as_deref(),
            Some("\u{434}")
        );
    }

    #[test]
    fn sheets_sharing_a_start_are_rejected() {
        let entry = |offset| SheetEntry {
            offset,
            name: String::new(),
            worksheet: true,
        };
        assert!(sheet_bounds(&[entry(10), entry(10)], 100).is_err());
        assert_eq!(
            sheet_bounds(&[entry(50), entry(10)], 100).unwrap(),
            [100, 50],
            "bounds follow stream order, not sheet order"
        );
    }

    #[test]
    fn rk_int_helper() {
        assert_eq!(rk_value(rk_int(-7)).to_bits(), (-7.0_f64).to_bits());
    }
}
