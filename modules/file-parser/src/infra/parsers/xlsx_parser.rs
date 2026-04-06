use crate::domain::error::DomainError;
use crate::domain::ir::{
    DocumentBuilder, Inline, ParsedBlock, ParsedSource, TableBlock, TableCell, TableRow,
};
use crate::domain::parser::FileParserBackend;
use async_trait::async_trait;
use calamine::{Data, Reader, Xls, Xlsb, Xlsx, open_workbook_auto};
use std::path::Path;

/// Excel file format variants
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ExcelFormat {
    /// Legacy Excel format (.xls)
    Xls,
    /// Excel Open XML format (.xlsx)
    #[default]
    Xlsx,
    /// Excel Open XML Macro-Enabled format (.xlsm)
    Xlsm,
    /// Excel Binary format (.xlsb)
    Xlsb,
}

/// OLE Compound Document magic bytes (used by .xls)
const OLE_MAGIC: [u8; 4] = [0xD0, 0xCF, 0x11, 0xE0];
/// ZIP archive magic bytes (used by .xlsx, .xlsm, .xlsb)
const ZIP_MAGIC: [u8; 4] = [0x50, 0x4B, 0x03, 0x04];

impl From<&str> for ExcelFormat {
    fn from(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "xls" => Self::Xls,
            "xlsx" => Self::Xlsx,
            "xlsm" => Self::Xlsm,
            "xlsb" => Self::Xlsb,
            _ => Self::default(),
        }
    }
}

impl ExcelFormat {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Xls => "xls",
            Self::Xlsx => "xlsx",
            Self::Xlsm => "xlsm",
            Self::Xlsb => "xlsb",
        }
    }
    /// Determine format from filename extension (used as hint for MIME type)
    /// Returns an error if the extension is not a recognized Excel format
    pub fn from_filename(filename: &str) -> Result<Self, DomainError> {
        let ext = Path::new(filename)
            .extension()
            .and_then(|os| os.to_str())
            .ok_or_else(|| {
                DomainError::parse_error(format!("Unrecognized Excel file extension: '{filename}'"))
            })?;

        match ext.to_lowercase().as_str() {
            "xls" => Ok(Self::Xls),
            "xlsx" => Ok(Self::Xlsx),
            "xlsm" => Ok(Self::Xlsm),
            "xlsb" => Ok(Self::Xlsb),
            _ => Err(DomainError::parse_error(format!(
                "Unrecognized Excel file extension: '{filename}'"
            ))),
        }
    }

    /// Detect format from file magic bytes
    /// Returns None if the bytes don't match any known Excel format
    #[must_use]
    pub fn from_magic_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 4 {
            return None;
        }

        let magic: [u8; 4] = [bytes[0], bytes[1], bytes[2], bytes[3]];

        if magic == OLE_MAGIC {
            // OLE format = legacy XLS
            Some(Self::Xls)
        } else if magic == ZIP_MAGIC {
            // ZIP-based format (xlsx, xlsm, or xlsb)
            // All three share the same magic bytes; we'll try parsers in order
            Some(Self::Xlsx)
        } else {
            None
        }
    }

    /// Get the MIME type for this format
    #[must_use]
    pub const fn mime_type(self) -> &'static str {
        match self {
            Self::Xls => "application/vnd.ms-excel",
            Self::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Self::Xlsm => "application/vnd.ms-excel.sheet.macroEnabled.12",
            Self::Xlsb => "application/vnd.ms-excel.sheet.binary.macroEnabled.12",
        }
    }

    /// Get all supported file extensions
    #[must_use]
    pub const fn all_extensions() -> &'static [&'static str] {
        const EXTENSIONS: &[&str] = &[
            ExcelFormat::Xlsx.as_str(),
            ExcelFormat::Xls.as_str(),
            ExcelFormat::Xlsm.as_str(),
            ExcelFormat::Xlsb.as_str(),
        ];
        EXTENSIONS
    }
}

/// XLSX/XLS parser that extracts data from Excel spreadsheets using calamine
pub struct XlsxParser;

impl XlsxParser {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for XlsxParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl FileParserBackend for XlsxParser {
    fn id(&self) -> &'static str {
        ExcelFormat::default().as_str()
    }

    fn supported_extensions(&self) -> &'static [&'static str] {
        ExcelFormat::all_extensions()
    }

    async fn parse_local_path(
        &self,
        path: &Path,
    ) -> Result<crate::domain::ir::ParsedDocument, DomainError> {
        let path_buf = path.to_path_buf();

        let blocks =
            tokio::task::spawn_blocking(move || -> Result<Vec<ParsedBlock>, DomainError> {
                parse_spreadsheet_from_path(&path_buf)
            })
            .await
            .map_err(|e| DomainError::parse_error(format!("Task join error: {e}")))??;

        let default_filename = format!("unknown.{}", ExcelFormat::default().as_str());
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&default_filename);
        let format =
            ExcelFormat::from_filename(filename).unwrap_or_else(|_| ExcelFormat::default());
        let content_type = format.mime_type();

        let mut builder = DocumentBuilder::new(ParsedSource::LocalPath(path.display().to_string()))
            .content_type(content_type)
            .blocks(blocks);

        if let Some(filename) = path.file_name().and_then(|s| s.to_str()) {
            builder = builder.title(filename).original_filename(filename);
        }

        Ok(builder.build())
    }

    async fn parse_bytes(
        &self,
        filename_hint: Option<&str>,
        _content_type: Option<&str>,
        bytes: bytes::Bytes,
    ) -> Result<crate::domain::ir::ParsedDocument, DomainError> {
        let default_filename = format!("unknown.{}", ExcelFormat::default().as_str());
        let filename = filename_hint.unwrap_or(&default_filename).to_owned();

        // Detect format from magic bytes first, fall back to filename
        let format = ExcelFormat::from_magic_bytes(bytes.as_ref())
            .or_else(|| ExcelFormat::from_filename(&filename).ok())
            .ok_or_else(|| {
                DomainError::parse_error(
                    "Could not determine Excel format from content or filename",
                )
            })?;
        let content_type = format.mime_type();

        let blocks =
            tokio::task::spawn_blocking(move || -> Result<Vec<ParsedBlock>, DomainError> {
                parse_spreadsheet_from_bytes(&bytes)
            })
            .await
            .map_err(|e| DomainError::parse_error(format!("Task join error: {e}")))??;

        let source = ParsedSource::Uploaded {
            original_name: filename.clone(),
        };

        let mut builder = DocumentBuilder::new(source)
            .content_type(content_type)
            .blocks(blocks);

        builder = builder.title(&filename).original_filename(&filename);

        Ok(builder.build())
    }
}

fn parse_spreadsheet_from_path(path: &Path) -> Result<Vec<ParsedBlock>, DomainError> {
    let mut workbook = open_workbook_auto(path)
        .map_err(|e| DomainError::parse_error(format!("Failed to open spreadsheet: {e}")))?;

    Ok(extract_blocks_from_workbook(&mut workbook))
}

fn parse_spreadsheet_from_bytes(bytes: &[u8]) -> Result<Vec<ParsedBlock>, DomainError> {
    // Detect format from magic bytes
    let format = ExcelFormat::from_magic_bytes(bytes)
        .ok_or_else(|| DomainError::parse_error("Unrecognized Excel file format"))?;

    match format {
        ExcelFormat::Xls => {
            let cursor = std::io::Cursor::new(bytes);
            let mut workbook: Xls<_> = Xls::new(cursor)
                .map_err(|e| DomainError::parse_error(format!("Failed to open XLS: {e}")))?;
            Ok(extract_blocks_from_workbook(&mut workbook))
        }
        ExcelFormat::Xlsx | ExcelFormat::Xlsm | ExcelFormat::Xlsb => {
            // All ZIP-based formats share the same magic bytes.
            // Try XLSX first (most common), then XLSB if that fails.
            let cursor = std::io::Cursor::new(bytes);
            if let Ok(mut workbook) = Xlsx::new(cursor) {
                return Ok(extract_blocks_from_workbook(&mut workbook));
            }

            let cursor = std::io::Cursor::new(bytes);
            if let Ok(mut workbook) = Xlsb::new(cursor) {
                return Ok(extract_blocks_from_workbook(&mut workbook));
            }

            Err(DomainError::parse_error(
                "Failed to parse as XLSX or XLSB format",
            ))
        }
    }
}

fn extract_blocks_from_workbook<RS: std::io::Read + std::io::Seek, R: Reader<RS>>(
    workbook: &mut R,
) -> Vec<ParsedBlock> {
    let mut blocks = Vec::new();
    let sheet_names = workbook.sheet_names();

    for sheet_name in sheet_names {
        // Add sheet name as a heading
        blocks.push(ParsedBlock::Heading {
            level: 2,
            inlines: vec![Inline::plain(&sheet_name)],
        });

        // Get the worksheet range
        let range = match workbook.worksheet_range(&sheet_name) {
            Ok(range) => range,
            Err(e) => {
                tracing::warn!("Failed to read sheet '{}': {:?}", sheet_name, e);
                continue;
            }
        };

        // Convert range to table block
        if let Some(table_block) = range_to_table_block(&range) {
            blocks.push(table_block);
        }
    }

    blocks
}

fn range_to_table_block(range: &calamine::Range<Data>) -> Option<ParsedBlock> {
    let height = range.height();
    let width = range.width();

    if height == 0 || width == 0 {
        return None;
    }

    let mut rows = Vec::with_capacity(height);

    for (row_idx, row) in range.rows().enumerate() {
        let is_header = row_idx == 0;
        let mut cells = Vec::with_capacity(width);

        for cell in row {
            let text = cell_to_string(cell);
            cells.push(TableCell {
                blocks: vec![ParsedBlock::Paragraph {
                    inlines: vec![Inline::plain(text)],
                }],
            });
        }

        rows.push(TableRow { is_header, cells });
    }

    Some(ParsedBlock::Table(TableBlock { rows }))
}

fn cell_to_string(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::String(s) | Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
        Data::Int(i) => i.to_string(),
        Data::Float(f) => {
            // Format floats nicely - remove trailing zeros
            if f.fract() == 0.0 {
                // For whole numbers, format without decimal point
                // Use format with .0 precision to get integer-like output
                format!("{f:.0}")
            } else {
                format!("{f}")
            }
        }
        Data::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_owned(),
        Data::DateTime(dt) => {
            // calamine DateTime is days since 1899-12-30
            format!("{dt}")
        }
        Data::Error(e) => format!("#ERROR: {e:?}"),
    }
}

#[cfg(test)]
#[path = "xlsx_parser_tests.rs"]
mod tests;
