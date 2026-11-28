//! Pieces shared by the XLSX, XLSB and XLS readers.

use std::collections::HashMap;

use crate::domain::error::DomainError;

use super::cells::{format_float, serial_text};
use super::numfmt::{self, Kind};

/// A shared string table stored as one buffer plus end offsets: an empty
/// string costs four bytes of index, not a whole `String`.
#[derive(Debug, Default)]
pub(super) struct SharedStrings {
    text: String,
    ends: Vec<u32>,
}

impl SharedStrings {
    pub(super) fn get(&self, index: usize) -> Option<&str> {
        let end = usize::try_from(*self.ends.get(index)?).ok()?;
        let start = match index.checked_sub(1) {
            Some(prev) => usize::try_from(*self.ends.get(prev)?).ok()?,
            None => 0,
        };
        self.text.get(start..end)
    }

    pub(super) fn push(&mut self, s: &str) -> Result<(), DomainError> {
        self.text.push_str(s);
        let end = u32::try_from(self.text.len()).map_err(|_| {
            DomainError::parse_error("Failed to parse spreadsheet: string table over 4 GiB")
        })?;
        self.ends.push(end);
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn to_vec(&self) -> Vec<&str> {
        (0..self.ends.len()).filter_map(|i| self.get(i)).collect()
    }
}

/// Display kind per cell format (XF) index, from the workbook's number formats.
#[derive(Debug, Default)]
pub(super) struct CellFormats {
    custom: HashMap<u32, Kind>,
    xf: Vec<Kind>,
}

impl CellFormats {
    /// Registers custom number format `id` with format code `code`.
    pub(super) fn add_format(&mut self, id: u32, code: &str) {
        self.custom.insert(id, numfmt::classify(code));
    }

    /// Registers the next cell format, which uses number format `id`.
    /// Formats are resolved when the XF is seen; Excel writes them first.
    pub(super) fn add_xf(&mut self, id: u32) {
        let kind = self
            .custom
            .get(&id)
            .copied()
            .unwrap_or_else(|| numfmt::builtin(id));
        self.xf.push(kind);
    }

    pub(super) fn kind(&self, xf: usize) -> Kind {
        self.xf.get(xf).copied().unwrap_or(Kind::Number)
    }
}

/// Text of a numeric cell shown through cell format `kind`.
pub(super) fn number_text(value: f64, kind: Kind, date1904: bool) -> String {
    match kind {
        Kind::Number => format_float(value),
        _ => serial_text(value, kind, date1904),
    }
}

/// The value of an `RkNumber` (BIFF8 RK records, BIFF12 `BrtCellRk`): a
/// 30-bit integer or the high 30 bits of a double, optionally divided by 100.
pub(super) fn rk_value(rk: u32) -> f64 {
    let divide = rk & 0x01 != 0;
    let value = if rk & 0x02 != 0 {
        // Arithmetic shift keeps the sign of the 30-bit integer.
        f64::from(i32::from_le_bytes(rk.to_le_bytes()) >> 2)
    } else {
        f64::from_bits(u64::from(rk & 0xFFFF_FFFC) << 32)
    };
    if divide { value / 100.0 } else { value }
}

/// Excel's text for a cell error code (BIFF8 and BIFF12 share the codes).
pub(super) fn error_text(code: u8) -> &'static str {
    match code {
        0x00 => "#NULL!",
        0x07 => "#DIV/0!",
        0x0F => "#VALUE!",
        0x17 => "#REF!",
        0x1D => "#NAME?",
        0x24 => "#NUM!",
        0x2A => "#N/A",
        0x2B => "#GETTING_DATA",
        _ => "#ERROR!",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rk_numbers() {
        // Integer 5: 5 << 2 | 0b10.
        assert_eq!(rk_value((5 << 2) | 0x02).to_bits(), (5.0_f64).to_bits());
        // Integer -3 divided by 100.
        let minus_three = u32::from_le_bytes((-3_i32 << 2).to_le_bytes());
        assert_eq!(
            rk_value(minus_three | 0x03).to_bits(),
            (-0.03_f64).to_bits()
        );
        // Double 1.5: high 32 bits of its IEEE form.
        let high = u32::try_from(1.5_f64.to_bits() >> 32).unwrap();
        assert_eq!(rk_value(high).to_bits(), (1.5_f64).to_bits());
    }

    #[test]
    fn errors() {
        assert_eq!(error_text(0x07), "#DIV/0!");
        assert_eq!(error_text(0x2A), "#N/A");
        assert_eq!(error_text(0x99), "#ERROR!");
    }

    #[test]
    fn cell_formats_resolve_custom_then_builtin() {
        let mut formats = CellFormats::default();
        formats.add_format(164, "yyyy-mm-dd");
        formats.add_xf(0);
        formats.add_xf(164);
        formats.add_xf(22);
        assert_eq!(formats.kind(0), Kind::Number);
        assert_eq!(formats.kind(1), Kind::Date);
        assert_eq!(formats.kind(2), Kind::DateTime);
        assert_eq!(formats.kind(99), Kind::Number);
    }

    #[test]
    fn shared_strings_index() {
        let mut s = SharedStrings::default();
        s.push("a").unwrap();
        s.push("").unwrap();
        s.push("bc").unwrap();
        assert_eq!(s.to_vec(), ["a", "", "bc"]);
        assert_eq!(s.get(3), None);
    }
}
