use super::{ExcelFormat, cell_to_string};
use calamine::Data;

#[test]
fn test_excel_format_from_filename_xls() {
    assert_eq!(
        ExcelFormat::from_filename("document.xls").expect("should parse .xls"),
        ExcelFormat::Xls
    );
    assert_eq!(
        ExcelFormat::from_filename("document.XLS").expect("should parse .XLS"),
        ExcelFormat::Xls
    );
}

#[test]
fn test_excel_format_from_filename_xlsx() {
    assert_eq!(
        ExcelFormat::from_filename("document.xlsx").expect("should parse .xlsx"),
        ExcelFormat::Xlsx
    );
    assert_eq!(
        ExcelFormat::from_filename("document.XLSX").expect("should parse .XLSX"),
        ExcelFormat::Xlsx
    );
}

#[test]
fn test_excel_format_from_filename_xlsm() {
    assert_eq!(
        ExcelFormat::from_filename("document.xlsm").expect("should parse .xlsm"),
        ExcelFormat::Xlsm
    );
    assert_eq!(
        ExcelFormat::from_filename("document.XLSM").expect("should parse .XLSM"),
        ExcelFormat::Xlsm
    );
}

#[test]
fn test_excel_format_from_filename_xlsb() {
    assert_eq!(
        ExcelFormat::from_filename("document.xlsb").expect("should parse .xlsb"),
        ExcelFormat::Xlsb
    );
    assert_eq!(
        ExcelFormat::from_filename("document.XLSB").expect("should parse .XLSB"),
        ExcelFormat::Xlsb
    );
}

#[test]
fn test_excel_format_from_filename_unknown() {
    assert!(ExcelFormat::from_filename("document.unknown").is_err());
    assert!(ExcelFormat::from_filename("document").is_err());
    assert!(ExcelFormat::from_filename("").is_err());
    assert!(ExcelFormat::from_filename("file.pdf").is_err());
}

#[test]
fn test_excel_format_mime_types() {
    assert_eq!(ExcelFormat::Xls.mime_type(), "application/vnd.ms-excel");
    assert_eq!(
        ExcelFormat::Xlsx.mime_type(),
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
    );
    assert_eq!(
        ExcelFormat::Xlsm.mime_type(),
        "application/vnd.ms-excel.sheet.macroEnabled.12"
    );
    assert_eq!(
        ExcelFormat::Xlsb.mime_type(),
        "application/vnd.ms-excel.sheet.binary.macroEnabled.12"
    );
}

#[test]
fn test_excel_format_extensions() {
    assert_eq!(ExcelFormat::Xls, "xls".into());
    assert_eq!(ExcelFormat::Xlsx, "xlsx".into());
    assert_eq!(ExcelFormat::Xlsm, "xlsm".into());
    assert_eq!(ExcelFormat::Xlsb, "xlsb".into());
}

#[test]
fn test_excel_format_all_extensions() {
    assert_eq!(
        ExcelFormat::all_extensions(),
        &["xlsx", "xls", "xlsm", "xlsb"]
    );
}

#[test]
fn test_excel_format_default() {
    assert_eq!(ExcelFormat::default(), ExcelFormat::Xlsx);
    assert_eq!(ExcelFormat::default(), "xlsx".into());
}

#[test]
fn test_from_magic_bytes_xls() {
    // OLE magic bytes: D0 CF 11 E0
    let xls_bytes = [0xD0, 0xCF, 0x11, 0xE0, 0x00, 0x00];
    assert_eq!(
        ExcelFormat::from_magic_bytes(&xls_bytes),
        Some(ExcelFormat::Xls)
    );
}

#[test]
fn test_from_magic_bytes_xlsx() {
    // ZIP magic bytes: 50 4B 03 04 (PK..)
    let xlsx_bytes = [0x50, 0x4B, 0x03, 0x04, 0x00, 0x00];
    assert_eq!(
        ExcelFormat::from_magic_bytes(&xlsx_bytes),
        Some(ExcelFormat::Xlsx)
    );
}

#[test]
fn test_from_magic_bytes_unknown() {
    // Random bytes
    let unknown_bytes = [0x00, 0x01, 0x02, 0x03];
    assert_eq!(ExcelFormat::from_magic_bytes(&unknown_bytes), None);
}

#[test]
fn test_from_magic_bytes_too_short() {
    let short_bytes = [0xD0, 0xCF];
    assert_eq!(ExcelFormat::from_magic_bytes(&short_bytes), None);
}

#[test]
fn test_from_magic_bytes_empty() {
    let empty: [u8; 0] = [];
    assert_eq!(ExcelFormat::from_magic_bytes(&empty), None);
}

#[test]
fn test_cell_to_string_empty() {
    assert_eq!(cell_to_string(&Data::Empty), "");
}

#[test]
fn test_cell_to_string_string() {
    assert_eq!(cell_to_string(&Data::String("hello".to_owned())), "hello");
}

#[test]
fn test_cell_to_string_int() {
    assert_eq!(cell_to_string(&Data::Int(42)), "42");
}

#[test]
fn test_cell_to_string_float() {
    assert_eq!(cell_to_string(&Data::Float(2.71)), "2.71");
    assert_eq!(cell_to_string(&Data::Float(42.0)), "42");
}

#[test]
fn test_cell_to_string_bool() {
    assert_eq!(cell_to_string(&Data::Bool(true)), "TRUE");
    assert_eq!(cell_to_string(&Data::Bool(false)), "FALSE");
}
