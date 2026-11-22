//! Reading OOXML packages (DOCX, PPTX) with ZIP-bomb and path-traversal limits.
//!
//! An OOXML file is a ZIP of XML parts that point at each other through
//! relationship (`.rels`) parts. [`Archive`] enforces [`ParseLimits`] on every
//! part it reads. Parts that are never read (media, embedded objects) are not
//! checked, so a well-compressed image cannot get a document rejected.
//!
//! Element and attribute names are matched by their qualified name (`w:p`,
//! `r:id`), using the prefixes ECMA-376 declares. This keeps `<p:sldId id=".."
//! r:id="..">` and `w:t` / `m:t` apart, which local-name matching does not.

pub mod xml;

use std::collections::HashMap;
use std::io::{Cursor, Read};

use bytes::Bytes;
use quick_xml::events::Event;
use zip::ZipArchive;

use crate::domain::error::{DomainError, ParseLimit};
use crate::infra::parsers::le_bytes::{u16_at, u32_at};

use super::limits::{
    DepthGuard, ParseLimits, finish_title, is_limit, limit_exceeded, push_title_text,
};

/// Longest part name or target echoed back in an error message; both come from
/// the uploaded file.
const MAX_ECHOED_NAME: usize = 256;

/// Signature that starts every ZIP central directory header.
const CENTRAL_HEADER_SIGNATURE: [u8; 4] = *b"PK\x01\x02";
/// Signature of the end of central directory record, and its fixed length.
const END_SIGNATURE: [u8; 4] = *b"PK\x05\x06";
const END_LEN: usize = 22;
/// Longest archive comment, which sits after the end record.
const MAX_COMMENT_LEN: usize = 0xFFFF;

/// Where the central directory starts, when the last end record leaves zip no
/// other choice: a plain (not ZIP64) single-disk record whose directory ends
/// right where the record starts and begins with a header. zip reads such a
/// directory, or fails, without trying another end record, and every header it
/// decodes lies between that start and the end of the file. `None` for
/// anything else, which is then scanned whole.
fn central_directory_start(bytes: &[u8]) -> Option<usize> {
    let window = bytes.len().saturating_sub(END_LEN + MAX_COMMENT_LEN);
    let end = window
        + bytes
            .get(window..)?
            .windows(END_SIGNATURE.len())
            .rposition(|w| w == END_SIGNATURE)?;
    let record = bytes.get(end..end.checked_add(END_LEN)?)?;
    let disk = u16_at(record, 4)?;
    let cd_disk = u16_at(record, 6)?;
    let on_disk = u16_at(record, 8)?;
    let entries = u16_at(record, 10)?;
    let size = u32_at(record, 12)?;
    let offset = u32_at(record, 16)?;
    let comment = usize::from(u16_at(record, 20)?);
    let plain = entries != u16::MAX && size != u32::MAX && offset != u32::MAX;
    let single_disk = disk == 0 && cd_disk == 0 && on_disk == entries;
    let start = usize::try_from(offset).ok()?;
    let abuts = start.checked_add(usize::try_from(size).ok()?) == Some(end);
    let fits = end + END_LEN + comment <= bytes.len();
    let header_at_start = bytes.get(start..start + 4) == Some(&CENTRAL_HEADER_SIGNATURE[..]);
    (plain && single_disk && entries > 0 && abuts && fits && header_at_start).then_some(start)
}

/// `Some(count)` once `bytes` holds more than `limit` central directory header
/// signatures. Every entry zip decodes starts with one, and headers do not
/// overlap, so this bounds the entry count whichever end record zip trusts.
/// Compressed data matches by chance about once per 4 GiB.
fn too_many_headers(bytes: &[u8], limit: usize) -> Option<usize> {
    let mut found = 0usize;
    for window in bytes.windows(CENTRAL_HEADER_SIGNATURE.len()) {
        if window == CENTRAL_HEADER_SIGNATURE {
            found += 1;
            if found > limit {
                return Some(found);
            }
        }
    }
    None
}

/// An opened OOXML package.
pub struct Archive {
    zip: ZipArchive<Cursor<Bytes>>,
    limits: ParseLimits,
    /// Decompressed bytes this archive may still hand out.
    read_budget: u64,
    /// ASCII-lower-cased entry name -> index of the first entry with that name.
    /// Part names compare case-insensitively (ASCII) in OPC, and producers do
    /// not always match the case their relationships use.
    by_lower_name: HashMap<String, usize>,
    format: &'static str,
    /// Length of the whole archive; no entry can hold more compressed bytes.
    byte_len: u64,
}

impl Archive {
    /// Opens `bytes` as a ZIP and checks the entry count against `limits`.
    ///
    /// `format` (`"DOCX"`, `"PPTX"`) only labels error messages.
    pub fn open(
        bytes: Bytes,
        format: &'static str,
        limits: &ParseLimits,
    ) -> Result<Self, DomainError> {
        // zip decodes every central directory entry, with no cap, before it
        // returns; each takes several times its 46-byte header in memory.
        // Only the directory is scanned when its place is certain, so a
        // stored embedded ZIP does not count against the package.
        let directory = central_directory_start(&bytes).map_or(&bytes[..], |start| &bytes[start..]);
        if let Some(found) = too_many_headers(directory, limits.max_zip_entries) {
            return Err(limit_exceeded(
                ParseLimit::MaxZipEntries,
                format!(
                    "at least {found} central directory headers, limit {}",
                    limits.max_zip_entries
                ),
            ));
        }
        let byte_len = bytes.len() as u64;
        let zip = ZipArchive::new(Cursor::new(bytes))
            .map_err(|e| DomainError::parse_error(format!("Failed to open {format} file: {e}")))?;

        if zip.len() > limits.max_zip_entries {
            return Err(limit_exceeded(
                ParseLimit::MaxZipEntries,
                format!("{} entries, limit {}", zip.len(), limits.max_zip_entries),
            ));
        }

        let mut by_lower_name = HashMap::with_capacity(zip.len());
        for index in 0..zip.len() {
            if let Some(Ok(name)) = zip.name_for_index(index) {
                by_lower_name
                    .entry(name.to_ascii_lowercase())
                    .or_insert(index);
            }
        }

        Ok(Self {
            zip,
            limits: limits.clone(),
            read_budget: limits.max_total_uncompressed_bytes,
            by_lower_name,
            format,
            byte_len,
        })
    }

    /// Reads the part `name` (a leading `/` is ignored), or `None` if the
    /// package has no such part.
    ///
    /// Every byte inflated counts against the archive's total budget, also when
    /// the read then fails, so repeated reads of a broken part stay bounded.
    pub fn read(&mut self, name: &str) -> Result<Option<Vec<u8>>, DomainError> {
        let Some(index) = self.index_of(name) else {
            return Ok(None);
        };
        let format = self.format;
        let shown = echo(name);
        let max_entry = self.limits.max_entry_bytes;
        let max_total = self.limits.max_total_uncompressed_bytes;
        let mut file = self.zip.by_index(index).map_err(|e| {
            DomainError::parse_error(format!("Failed to parse {format}: part {shown}: {e}"))
        })?;

        let declared = file.size();
        if declared > max_entry {
            return Err(limit_exceeded(
                ParseLimit::MaxEntryBytes,
                format!("part {shown} declares {declared} bytes, limit {max_entry}"),
            ));
        }
        // The declared compressed size is not checked against the archive by
        // zip; an entry cannot read more than what follows its data start.
        let available = file
            .data_start()
            .map_or(self.byte_len, |start| self.byte_len.saturating_sub(start));
        let compressed = file.compressed_size().min(available);
        check_ratio(&shown, declared, compressed, &self.limits)?;
        if declared > self.read_budget {
            return Err(budget_exceeded(max_total));
        }

        // Pay for the declared size up front: zip 9 fails any read that
        // inflates past it, and a failed read gives back no byte count, so
        // charging afterwards would let a broken part be inflated for free.
        self.read_budget -= declared;
        // The cap keeps the limits if zip ever stops enforcing the declared
        // size: read at most one byte past them and treat that byte as overflow.
        // zip 9 fails such reads first, so the two checks after the read are
        // defence in depth and no test reaches them.
        let cap = max_entry.min(self.read_budget.saturating_add(declared));
        let mut buf = Vec::new();
        let result = (&mut file)
            .take(cap.saturating_add(1))
            .read_to_end(&mut buf);
        let read = buf.len() as u64;
        self.read_budget = self
            .read_budget
            .saturating_sub(read.saturating_sub(declared));
        result.map_err(|e| {
            DomainError::parse_error(format!("Failed to parse {format}: part {shown}: {e}"))
        })?;
        if read > max_entry {
            return Err(limit_exceeded(
                ParseLimit::MaxEntryBytes,
                format!("part {shown} inflates past {max_entry} bytes"),
            ));
        }
        if read > cap {
            return Err(budget_exceeded(max_total));
        }
        Ok(Some(buf))
    }

    /// Reads the part `name` and decodes it to text (UTF-8, or UTF-16 with a
    /// BOM). The decoded text is held to the per-part limit as well: lossy
    /// decoding of garbage can triple its size.
    ///
    /// The size is measured before the text is built, and what decoding adds
    /// is charged to the archive's total budget.
    pub fn read_xml(&mut self, name: &str) -> Result<Option<String>, DomainError> {
        let Some(bytes) = self.read(name)? else {
            return Ok(None);
        };
        let decoded = xml::decoded_len(&bytes) as u64;
        if decoded > self.limits.max_entry_bytes {
            return Err(limit_exceeded(
                ParseLimit::MaxEntryBytes,
                format!(
                    "part {} decodes past {} bytes",
                    echo(name),
                    self.limits.max_entry_bytes
                ),
            ));
        }
        let growth = decoded.saturating_sub(bytes.len() as u64);
        self.read_budget = self
            .read_budget
            .checked_sub(growth)
            .ok_or_else(|| budget_exceeded(self.limits.max_total_uncompressed_bytes))?;
        Ok(Some(xml::decode_part(bytes)))
    }

    /// Reads the relationships of `part`, e.g. `word/_rels/document.xml.rels`
    /// for `word/document.xml`. A part without a `.rels` file has none.
    pub fn read_rels(&mut self, part: &str) -> Result<Relationships, DomainError> {
        let rels_name = rels_part_for(part);
        match self.read_xml(&rels_name)? {
            Some(text) => Relationships::parse(&text, part, &rels_name, self.format, &self.limits),
            None => Ok(Relationships::default()),
        }
    }

    /// Whether the package has a part called `name`.
    pub fn contains(&self, name: &str) -> bool {
        self.index_of(name).is_some()
    }

    /// The limits this archive was opened with.
    pub fn limits(&self) -> &ParseLimits {
        &self.limits
    }

    fn index_of(&self, name: &str) -> Option<usize> {
        let name = name.strip_prefix('/').unwrap_or(name);
        self.zip
            .index_for_name(name)
            .or_else(|| self.by_lower_name.get(&name.to_ascii_lowercase()).copied())
    }
}

/// XML events of one part, with element depth held to
/// [`ParseLimits::max_nesting_depth`].
pub struct Events<'a> {
    reader: quick_xml::Reader<&'a [u8]>,
    depth: DepthGuard,
    format: &'static str,
}

impl<'a> Events<'a> {
    /// `format` (`"PPTX"`) only labels error messages.
    pub fn new(text: &'a str, limits: &ParseLimits, format: &'static str) -> Self {
        Self {
            reader: xml::reader(text),
            depth: DepthGuard::new(limits),
            format,
        }
    }

    pub fn next(&mut self) -> Result<Event<'a>, DomainError> {
        let event = self.reader.read_event().map_err(|e| {
            DomainError::parse_error(format!("Failed to parse {}: {e}", self.format))
        })?;
        match &event {
            Event::Start(_) => self.depth.enter()?,
            Event::End(_) => self.depth.leave(),
            _ => {}
        }
        Ok(event)
    }
}

/// Conventional place of the core properties part.
const CORE_PART: &str = "docProps/core.xml";

/// The package's main part: the first internal `officeDocument` target that
/// the package has, else the first of `fallbacks` it has. `root` is the
/// package relationships (`archive.read_rels("")`).
pub fn main_part(archive: &Archive, root: &Relationships, fallbacks: &[&str]) -> Option<String> {
    root.of_type("officeDocument")
        .find(|r| !r.external && archive.contains(&r.target))
        .map(|r| r.target.clone())
        .or_else(|| {
            fallbacks
                .iter()
                .find(|part| archive.contains(part))
                .map(|&part| part.to_owned())
        })
}

/// The package title: `dc:title` of the core properties part, found like the
/// main part, with `docProps/core.xml` as the fallback.
pub fn package_title(
    archive: &mut Archive,
    root: &Relationships,
) -> Result<Option<String>, DomainError> {
    let part = root
        .of_type("core-properties")
        .find(|r| !r.external && archive.contains(&r.target))
        .map(|r| r.target.clone())
        .or_else(|| archive.contains(CORE_PART).then(|| CORE_PART.to_owned()));
    match part {
        Some(part) => core_title(archive, &part),
        None => Ok(None),
    }
}

/// `dc:title` from the core properties part `part`, whitespace collapsed;
/// `None` when it is missing, blank, malformed or unreadable. Metadata is not
/// worth failing a document for, but the archive's read limits still apply.
pub fn core_title(archive: &mut Archive, part: &str) -> Result<Option<String>, DomainError> {
    let format = archive.format;
    let Some(text) = tolerate(archive.read_xml(part), format, "core properties")? else {
        return Ok(None);
    };
    let limits = archive.limits().clone();
    let mut events = Events::new(&text, &limits, archive.format);
    let mut in_title = false;
    let mut title = String::new();
    loop {
        let Ok(event) = events.next() else {
            return Ok(None);
        };
        match event {
            Event::Start(e) if xml::local_name(&e) == "title" => in_title = true,
            Event::End(e) if in_title && e.local_name().as_ref() == "title" => break,
            Event::Text(t) if in_title => push_title_text(&mut title, &t.xml10_content()),
            Event::GeneralRef(r) if in_title => {
                push_title_text(&mut title, &xml::resolve_ref(&r).unwrap_or_default());
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(finish_title(&title))
}

/// Open `mc:AlternateContent` elements. The first branch of each is read;
/// the others repeat it for older readers and are skipped.
#[derive(Debug, Default)]
pub struct Alternates {
    taken: Vec<bool>,
}

/// What a walker does with an element as far as alternate content goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AltAction {
    /// Not an alternate-content element.
    Other,
    /// An alternate-content element; nothing more to do with it.
    Consumed,
    /// A branch after the one taken: skip the element and its content.
    SkipElement,
}

impl Alternates {
    /// Handles a start (or empty) element `name`.
    pub fn start(&mut self, name: &str, is_start: bool) -> AltAction {
        match name {
            "mc:AlternateContent" => {
                if is_start {
                    self.taken.push(false);
                }
                AltAction::Consumed
            }
            "mc:Choice" | "mc:Fallback" => {
                if is_start {
                    match self.taken.last_mut() {
                        Some(taken) if *taken => return AltAction::SkipElement,
                        Some(taken) => *taken = true,
                        None => {}
                    }
                }
                AltAction::Consumed
            }
            _ => AltAction::Other,
        }
    }

    /// Handles an end element; whether it closed an `mc:AlternateContent`.
    pub fn end(&mut self, name: &str) -> bool {
        let closes = name == "mc:AlternateContent";
        if closes {
            self.taken.pop();
        }
        closes
    }
}

/// `result`, or an empty value when an optional part is malformed: a broken
/// slide, layout, style or relationships part costs only its own content.
/// Limit breaches still fail the document. `format` and `what` label the
/// warning.
pub fn tolerate<T: Default>(
    result: Result<T, DomainError>,
    format: &str,
    what: &str,
) -> Result<T, DomainError> {
    match result {
        Err(e) if !is_limit(&e) => {
            tracing::warn!(format, part = what, error = %e, "skipping malformed optional part");
            Ok(T::default())
        }
        other => other,
    }
}

fn budget_exceeded(max_total: u64) -> DomainError {
    limit_exceeded(
        ParseLimit::MaxTotalUncompressedBytes,
        format!("parts inflate past {max_total} bytes"),
    )
}

/// `name` quoted and cut to [`MAX_ECHOED_NAME`] characters for error messages.
fn echo(name: &str) -> String {
    if name.chars().count() <= MAX_ECHOED_NAME {
        format!("{name:?}")
    } else {
        let cut: String = name.chars().take(MAX_ECHOED_NAME).collect();
        format!("{cut:?}...")
    }
}

/// Rejects a part whose declared inflation ratio marks it as a ZIP bomb.
fn check_ratio(
    shown: &str,
    declared: u64,
    compressed: u64,
    limits: &ParseLimits,
) -> Result<(), DomainError> {
    if declared < limits.ratio_check_floor_bytes {
        return Ok(());
    }
    // No compressor turns zero bytes into data; an entry claiming so is forged.
    let too_high =
        compressed == 0 || declared > compressed.saturating_mul(limits.max_compression_ratio);
    if too_high {
        return Err(limit_exceeded(
            ParseLimit::MaxCompressionRatio,
            format!(
                "part {shown} inflates {compressed} -> {declared} bytes, limit {}:1",
                limits.max_compression_ratio
            ),
        ));
    }
    Ok(())
}

/// `dir/name.xml` -> `dir/_rels/name.xml.rels`.
pub fn rels_part_for(part: &str) -> String {
    let part = part.strip_prefix('/').unwrap_or(part);
    match part.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

/// One relationship from a `.rels` part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relationship {
    pub id: String,
    pub rel_type: String,
    /// For internal targets, the resolved part name inside the package; for
    /// external targets, the target exactly as written (usually a URL).
    pub target: String,
    pub external: bool,
}

impl Relationship {
    /// Whether the type URI ends with `/{suffix}` (e.g. `slide`).
    pub fn is_type(&self, suffix: &str) -> bool {
        self.rel_type.rsplit('/').next() == Some(suffix)
    }
}

/// The relationships of one part, in document order.
#[derive(Debug, Default)]
pub struct Relationships {
    items: Vec<Relationship>,
    by_id: HashMap<String, usize>,
}

impl Relationships {
    fn parse(
        text: &str,
        source_part: &str,
        rels_part: &str,
        format: &str,
        limits: &ParseLimits,
    ) -> Result<Self, DomainError> {
        let mut reader = xml::reader(text);
        let mut depth = DepthGuard::new(limits);
        let mut rels = Self::default();
        loop {
            let event = reader.read_event().map_err(|e| {
                DomainError::parse_error(format!(
                    "Failed to parse {format}: malformed relationships part {}: {e}",
                    echo(rels_part)
                ))
            })?;
            match event {
                Event::Start(e) => {
                    depth.enter()?;
                    rels.push(relationship(&e, source_part));
                }
                Event::Empty(e) => rels.push(relationship(&e, source_part)),
                Event::End(_) => depth.leave(),
                Event::Eof => break,
                _ => {}
            }
        }
        Ok(rels)
    }

    fn push(&mut self, rel: Option<Relationship>) {
        // A duplicate Id keeps the first occurrence, like the lookup by name.
        if let Some(rel) = rel
            && !self.by_id.contains_key(&rel.id)
        {
            self.by_id.insert(rel.id.clone(), self.items.len());
            self.items.push(rel);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&Relationship> {
        self.by_id.get(id).and_then(|&i| self.items.get(i))
    }

    /// Target of the first internal relationship of type `suffix`.
    pub fn internal(&self, suffix: &str) -> Option<String> {
        self.of_type(suffix)
            .find(|r| !r.external)
            .map(|r| r.target.clone())
    }

    /// Relationships whose type URI ends with `/{suffix}` (e.g. `slide`), in
    /// document order.
    pub fn of_type<'a>(&'a self, suffix: &'a str) -> impl Iterator<Item = &'a Relationship> + 'a {
        self.items.iter().filter(move |rel| rel.is_type(suffix))
    }
}

fn relationship(e: &quick_xml::events::BytesStart<'_>, source_part: &str) -> Option<Relationship> {
    if xml::qname(e) != "Relationship" {
        return None;
    }
    let (Some(id), Some(raw_target)) = (xml::attr(e, "Id"), xml::attr(e, "Target")) else {
        tracing::debug!(source_part, "relationship without Id or Target ignored");
        return None;
    };
    let rel_type = xml::attr(e, "Type").unwrap_or_default();
    let external = xml::attr(e, "TargetMode").is_some_and(|m| m.eq_ignore_ascii_case("External"));
    let target = if external {
        raw_target
    } else {
        // A target that escapes the package is dropped, not fatal: the rest of
        // the document is still readable.
        match resolve_target(source_part, &raw_target) {
            Ok(target) => target,
            Err(e) => {
                tracing::debug!(source_part, id, error = %e, "relationship target ignored");
                return None;
            }
        }
    };
    Some(Relationship {
        id,
        rel_type,
        target,
        external,
    })
}

/// Resolves an internal relationship target against the part that owns it.
///
/// `/x/y.xml` is relative to the package root; anything else is relative to the
/// source part's directory. Rejects NUL bytes, drive or UNC prefixes and paths
/// that climb above the root.
pub fn resolve_target(source_part: &str, target: &str) -> Result<String, DomainError> {
    let invalid = |why: &str| {
        DomainError::parse_error(format!("Invalid part reference {}: {why}", echo(target)))
    };
    if target.contains('\0') {
        return Err(invalid("NUL byte"));
    }
    let target = target.replace('\\', "/");
    let bytes = target.as_bytes();
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if drive || target.starts_with("//") {
        return Err(invalid("drive or UNC prefix"));
    }

    let mut stack: Vec<&str> = Vec::new();
    let (base, rest) = match target.strip_prefix('/') {
        Some(rest) => ("", rest),
        None => (
            source_part
                .strip_prefix('/')
                .unwrap_or(source_part)
                .rsplit_once('/')
                .map_or("", |(dir, _)| dir),
            target.as_str(),
        ),
    };
    for segment in base.split('/').chain(rest.split('/')) {
        match segment {
            "" | "." => {}
            ".." => {
                if stack.pop().is_none() {
                    return Err(invalid("escapes the package root"));
                }
            }
            other => stack.push(other),
        }
    }
    if stack.is_empty() {
        return Err(invalid("empty path"));
    }
    Ok(stack.join("/"))
}

#[cfg(test)]
pub mod test_zip {
    //! Builds small in-memory ZIPs for parser tests.

    use std::io::{Cursor, Write};

    use zip::CompressionMethod;
    use zip::write::{SimpleFileOptions, ZipWriter};

    /// A ZIP with `parts` (name, content), deflated.
    pub fn build(parts: &[(&str, &[u8])]) -> bytes::Bytes {
        build_with(parts, CompressionMethod::Deflated)
    }

    /// A ZIP with `parts` stored or deflated as given.
    pub fn build_with(parts: &[(&str, &[u8])], method: CompressionMethod) -> bytes::Bytes {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(method);
        for (name, content) in parts {
            writer.start_file(*name, options).expect("start_file");
            writer.write_all(content).expect("write_all");
        }
        bytes::Bytes::from(writer.finish().expect("finish").into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::CompressionMethod;

    fn limits() -> ParseLimits {
        ParseLimits::default()
    }

    #[test]
    fn not_a_zip_is_reported_with_format() {
        let err = Archive::open(Bytes::from_static(b"not a zip"), "DOCX", &limits())
            .err()
            .expect("must fail");
        assert!(
            err.to_string().contains("Failed to open DOCX file"),
            "{err}"
        );
    }

    #[test]
    fn a_huge_core_title_is_cut() {
        let core = format!(
            "<cp:coreProperties xmlns:cp=\"c\" xmlns:dc=\"d\"><dc:title>{}</dc:title></cp:coreProperties>",
            "t".repeat(200_000)
        );
        let bytes = test_zip::build_with(
            &[("docProps/core.xml", core.as_bytes())],
            CompressionMethod::Stored,
        );
        let mut archive = Archive::open(bytes, "DOCX", &limits()).unwrap();
        let title = core_title(&mut archive, "docProps/core.xml")
            .unwrap()
            .expect("a title");
        assert_eq!(
            title,
            "t".repeat(crate::infra::parsers::limits::MAX_TITLE_CHARS)
        );
    }

    #[test]
    fn missing_part_is_none() {
        let bytes = test_zip::build(&[("a.xml", b"<a/>")]);
        let mut archive = Archive::open(bytes, "DOCX", &limits()).unwrap();
        assert_eq!(archive.read("b.xml").unwrap(), None);
        assert_eq!(
            archive.read("a.xml").unwrap().as_deref(),
            Some(&b"<a/>"[..])
        );
    }

    #[test]
    fn case_insensitive_lookup_and_leading_slash() {
        let bytes = test_zip::build(&[("Word/Document.xml", b"<d/>")]);
        let mut archive = Archive::open(bytes, "DOCX", &limits()).unwrap();
        assert!(archive.read("/word/document.xml").unwrap().is_some());
    }

    #[test]
    fn too_many_entries() {
        let names: Vec<String> = (0..4).map(|i| format!("p{i}.xml")).collect();
        let parts: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b"x"[..])).collect();
        let bytes = test_zip::build(&parts);
        let limits = ParseLimits {
            max_zip_entries: 3,
            ..limits()
        };
        let err = Archive::open(bytes, "DOCX", &limits)
            .err()
            .expect("must fail");
        assert!(err.to_string().contains("max_zip_entries"), "{err}");
    }

    #[test]
    fn entry_count_is_checked_before_the_central_directory_is_decoded() {
        // Not a ZIP zip could open: only the header count can reject it.
        let bytes = Bytes::from(b"PK\x01\x02".repeat(4));
        let limits = ParseLimits {
            max_zip_entries: 3,
            ..limits()
        };
        let err = Archive::open(bytes, "DOCX", &limits)
            .err()
            .expect("must fail");
        assert!(err.to_string().contains("max_zip_entries"), "{err}");
        assert!(too_many_headers(&b"PK\x01\x02".repeat(3), 3).is_none());
    }

    #[test]
    fn entry_over_cap_is_rejected_on_read() {
        let bytes = test_zip::build_with(&[("big.xml", &[b'a'; 64])], CompressionMethod::Stored);
        let limits = ParseLimits {
            max_entry_bytes: 32,
            ..limits()
        };
        let mut archive = Archive::open(bytes, "DOCX", &limits).unwrap();
        let err = archive.read("big.xml").unwrap_err();
        assert!(err.to_string().contains("max_entry_bytes"), "{err}");
    }

    #[test]
    fn zip_bomb_ratio_is_rejected_when_read() {
        // 2 MiB of zeros deflates to a few KiB: far beyond 100:1.
        let zeros = vec![0_u8; 2 * 1024 * 1024];
        let bytes = test_zip::build(&[("bomb.xml", &zeros), ("ok.xml", b"<a/>")]);
        let mut archive = Archive::open(bytes, "DOCX", &limits()).unwrap();
        // Parts that are never read do not get the document rejected.
        assert!(archive.read("ok.xml").unwrap().is_some());
        let err = archive.read("bomb.xml").unwrap_err();
        assert!(err.to_string().contains("max_compression_ratio"), "{err}");
    }

    #[test]
    fn small_entry_with_high_ratio_is_allowed() {
        let zeros = vec![0_u8; 64 * 1024];
        let bytes = test_zip::build(&[("small.xml", &zeros)]);
        let mut archive = Archive::open(bytes, "DOCX", &limits()).unwrap();
        assert_eq!(
            archive.read("small.xml").unwrap().map(|b| b.len()),
            Some(64 * 1024)
        );
    }

    #[test]
    fn forged_zero_compressed_size_is_rejected() {
        let limits = limits();
        let err = check_ratio("\"x\"", limits.ratio_check_floor_bytes, 0, &limits).unwrap_err();
        assert!(err.to_string().contains("max_compression_ratio"), "{err}");
    }

    #[test]
    fn total_budget_spans_parts() {
        let bytes = test_zip::build_with(
            &[("a.xml", &[b'a'; 40]), ("b.xml", &[b'b'; 40])],
            CompressionMethod::Stored,
        );
        let limits = ParseLimits {
            max_total_uncompressed_bytes: 64,
            ..limits()
        };
        let mut archive = Archive::open(bytes, "DOCX", &limits).unwrap();
        archive.read("a.xml").unwrap();
        let err = archive.read("b.xml").unwrap_err();
        assert!(
            err.to_string().contains("max_total_uncompressed_bytes"),
            "{err}"
        );
    }

    /// Rewrites the uncompressed-size field of every local and central
    /// directory header, so the archive lies about how big its entry is.
    fn forge_uncompressed_size(bytes: &[u8], size: u32) -> Bytes {
        let mut out = bytes.to_vec();
        let mut i = 0;
        while i + 4 <= out.len() {
            let field = match out[i..i + 4] {
                [0x50, 0x4B, 0x03, 0x04] => Some(i + 22),
                [0x50, 0x4B, 0x01, 0x02] => Some(i + 24),
                _ => None,
            };
            if let Some(at) = field {
                out[at..at + 4].copy_from_slice(&size.to_le_bytes());
            }
            i += 1;
        }
        Bytes::from(out)
    }

    /// Rewrites the compressed-size field of every local and central
    /// directory header.
    fn forge_compressed_size(bytes: &[u8], size: u32) -> Bytes {
        let mut out = bytes.to_vec();
        let mut i = 0;
        while i + 4 <= out.len() {
            let field = match out[i..i + 4] {
                [0x50, 0x4B, 0x03, 0x04] => Some(i + 18),
                [0x50, 0x4B, 0x01, 0x02] => Some(i + 20),
                _ => None,
            };
            if let Some(at) = field {
                out[at..at + 4].copy_from_slice(&size.to_le_bytes());
            }
            i += 1;
        }
        Bytes::from(out)
    }

    #[test]
    fn forged_compressed_size_does_not_pass_the_ratio_check() {
        // 2 MiB of zeros deflate to a few KiB; the entry claims 1 GiB.
        let zeros = vec![0_u8; 2 * 1024 * 1024];
        let honest = test_zip::build(&[("bomb.xml", &zeros)]);
        let forged = forge_compressed_size(&honest, 1 << 30);
        let mut archive = Archive::open(forged, "DOCX", &limits()).unwrap();
        let err = archive.read("bomb.xml").unwrap_err();
        assert!(err.to_string().contains("max_compression_ratio"), "{err}");
    }

    #[test]
    fn a_stored_inner_zip_does_not_count_against_the_entry_limit() {
        let names: Vec<String> = (0..20).map(|i| format!("p{i}.xml")).collect();
        let inner_parts: Vec<(&str, &[u8])> =
            names.iter().map(|n| (n.as_str(), &b"x"[..])).collect();
        let inner = test_zip::build_with(&inner_parts, CompressionMethod::Stored);
        let outer = test_zip::build_with(
            &[("a.xml", b"<a/>"), ("embedded.zip", &inner)],
            CompressionMethod::Stored,
        );
        let limits = ParseLimits {
            max_zip_entries: 10,
            ..limits()
        };
        let mut archive = Archive::open(outer.clone(), "DOCX", &limits).unwrap();
        assert!(archive.read("a.xml").unwrap().is_some());

        // Without a usable end record the whole file is scanned, as before.
        let cut = outer.slice(..outer.len() - 1);
        let err = Archive::open(cut, "DOCX", &limits)
            .err()
            .expect("must fail");
        assert!(err.to_string().contains("max_zip_entries"), "{err}");
    }

    #[test]
    fn entry_inflating_past_its_declared_size_is_rejected() {
        let real = vec![b'x'; 4096];
        let honest = test_zip::build(&[("lie.xml", &real)]);
        let lying = forge_uncompressed_size(&honest, 16);
        let mut archive = Archive::open(lying, "DOCX", &limits()).unwrap();
        let err = archive.read("lie.xml").unwrap_err();
        // zip's own size check fails the read, before the cap would.
        assert!(!is_limit(&err), "{err}");
        assert!(err.to_string().contains("lie.xml"), "{err}");
        assert!(
            err.to_string()
                .contains("larger than its declared uncompressed size"),
            "{err}"
        );
    }

    #[test]
    fn failed_read_still_spends_budget() {
        let real = vec![b'x'; 4096];
        let lying = forge_uncompressed_size(&test_zip::build(&[("lie.xml", &real)]), 16);
        let limits = ParseLimits {
            max_total_uncompressed_bytes: 40,
            ..limits()
        };
        let mut archive = Archive::open(lying, "DOCX", &limits).unwrap();
        // Each failing read pays the declared 16 bytes; the third attempt finds
        // only 8 left and is refused before inflating anything.
        assert!(archive.read("lie.xml").is_err());
        assert!(archive.read("lie.xml").is_err());
        let err = archive.read("lie.xml").unwrap_err();
        assert!(
            err.to_string().contains("max_total_uncompressed_bytes"),
            "{err}"
        );
    }

    #[test]
    fn exact_name_wins_over_case_variant() {
        let bytes = test_zip::build(&[
            ("Word/Document.xml", b"upper"),
            ("word/document.xml", b"lower"),
        ]);
        let mut archive = Archive::open(bytes, "DOCX", &limits()).unwrap();
        assert_eq!(
            archive.read("word/document.xml").unwrap().as_deref(),
            Some(&b"lower"[..])
        );
        assert_eq!(
            archive.read("Word/Document.xml").unwrap().as_deref(),
            Some(&b"upper"[..])
        );
        assert_eq!(
            archive.read("WORD/DOCUMENT.XML").unwrap().as_deref(),
            Some(&b"upper"[..])
        );
    }

    #[test]
    fn decoded_text_is_held_to_entry_limit() {
        // Every invalid byte becomes a 3-byte U+FFFD.
        let garbage = vec![0xFF_u8; 30];
        let bytes = test_zip::build_with(&[("g.xml", &garbage)], CompressionMethod::Stored);
        let limits = ParseLimits {
            max_entry_bytes: 40,
            ..limits()
        };
        let mut archive = Archive::open(bytes, "DOCX", &limits).unwrap();
        let err = archive.read_xml("g.xml").unwrap_err();
        assert!(err.to_string().contains("max_entry_bytes"), "{err}");
    }

    #[test]
    fn decoding_growth_is_charged_to_the_total_budget() {
        // 30 invalid bytes decode to 90; with 20 more the total passes 100.
        let bytes = test_zip::build_with(
            &[("g.xml", &[0xFF_u8; 30]), ("h.xml", &[b'h'; 20])],
            CompressionMethod::Stored,
        );
        let limits = ParseLimits {
            max_entry_bytes: 100,
            max_total_uncompressed_bytes: 100,
            ..limits()
        };
        let mut archive = Archive::open(bytes, "DOCX", &limits).unwrap();
        assert_eq!(
            archive.read_xml("g.xml").unwrap().map(|t| t.len()),
            Some(90)
        );
        let err = archive.read_xml("h.xml").unwrap_err();
        assert!(
            err.to_string().contains("max_total_uncompressed_bytes"),
            "{err}"
        );
    }

    #[test]
    fn an_unreadable_core_part_gives_no_title() {
        let core = b"<cp:coreProperties xmlns:cp=\"c\" xmlns:dc=\"d\"><dc:title>Quarterly</dc:title></cp:coreProperties>";
        let honest =
            test_zip::build_with(&[("docProps/core.xml", core)], CompressionMethod::Stored);
        // Change the stored text: the CRC no longer matches.
        let mut corrupt = honest.to_vec();
        let at = corrupt
            .windows(9)
            .position(|w| w == b"Quarterly")
            .expect("stored title");
        corrupt[at] = b'q';
        let mut archive = Archive::open(Bytes::from(corrupt), "DOCX", &limits()).unwrap();
        assert!(archive.read(CORE_PART).is_err(), "the read itself fails");
        assert_eq!(
            package_title(&mut archive, &Relationships::default()).unwrap(),
            None
        );

        // A limit breach still fails the document.
        let limits = ParseLimits {
            max_entry_bytes: 16,
            ..limits()
        };
        let mut archive = Archive::open(honest, "DOCX", &limits).unwrap();
        let err = package_title(&mut archive, &Relationships::default()).unwrap_err();
        assert!(is_limit(&err), "{err}");
    }

    #[test]
    fn a_core_title_is_collapsed_to_one_line() {
        let core = "<cp:coreProperties xmlns:cp=\"c\" xmlns:dc=\"d\"><dc:title> a\n\n---\r\nlanguage: x </dc:title></cp:coreProperties>";
        let bytes = test_zip::build(&[("docProps/core.xml", core.as_bytes())]);
        let mut archive = Archive::open(bytes, "DOCX", &limits()).unwrap();
        assert_eq!(
            core_title(&mut archive, CORE_PART).unwrap().as_deref(),
            Some("a --- language: x")
        );
    }

    #[test]
    fn malformed_rels_part_is_an_error_naming_the_part() {
        let bytes = test_zip::build(&[("word/_rels/document.xml.rels", b"<Relationships><a></b>")]);
        let mut archive = Archive::open(bytes, "DOCX", &limits()).unwrap();
        let err = archive.read_rels("word/document.xml").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("DOCX") && msg.contains("document.xml.rels"),
            "{msg}"
        );
    }

    #[test]
    fn read_budget_is_shared_across_reads() {
        let bytes = test_zip::build_with(&[("a.xml", &[b'a'; 40])], CompressionMethod::Stored);
        let limits = ParseLimits {
            max_total_uncompressed_bytes: 60,
            ..limits()
        };
        let mut archive = Archive::open(bytes, "DOCX", &limits).unwrap();
        archive.read("a.xml").unwrap();
        let err = archive.read("a.xml").unwrap_err();
        assert!(
            err.to_string().contains("max_total_uncompressed_bytes"),
            "{err}"
        );
    }

    #[test]
    fn contains_is_case_insensitive() {
        let bytes = test_zip::build(&[("xl/sheet.xml", b"<a/>")]);
        let archive = Archive::open(bytes, "XLSX", &limits()).unwrap();
        assert!(archive.contains("XL/SHEET.XML"));
        assert!(!archive.contains("xl/other.xml"));
    }

    #[test]
    fn rels_part_name() {
        assert_eq!(
            rels_part_for("word/document.xml"),
            "word/_rels/document.xml.rels"
        );
        assert_eq!(
            rels_part_for("/ppt/slides/slide1.xml"),
            "ppt/slides/_rels/slide1.xml.rels"
        );
        assert_eq!(
            rels_part_for("[Content_Types].xml"),
            "_rels/[Content_Types].xml.rels"
        );
    }

    #[test]
    fn resolve_relative_and_absolute_targets() {
        assert_eq!(
            resolve_target("ppt/slides/slide1.xml", "../media/image1.png").unwrap(),
            "ppt/media/image1.png"
        );
        assert_eq!(
            resolve_target("word/document.xml", "styles.xml").unwrap(),
            "word/styles.xml"
        );
        assert_eq!(
            resolve_target("word/document.xml", "/word/numbering.xml").unwrap(),
            "word/numbering.xml"
        );
        assert_eq!(
            resolve_target("word/document.xml", "media\\image.png").unwrap(),
            "word/media/image.png"
        );
    }

    #[test]
    fn resolve_rejects_escape_nul_drive_and_unc() {
        for bad in [
            "../../etc/passwd",
            "..\\..\\x",
            "a\0b",
            "C:/x.xml",
            "//server/share/x",
            "/",
        ] {
            assert!(resolve_target("word/document.xml", bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn rels_keep_external_targets_and_resolve_internal_ones() {
        let rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com/a?b=1&amp;c=2" TargetMode="External"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../../../escape.png"/>
</Relationships>"#;
        let bytes = test_zip::build(&[("word/_rels/document.xml.rels", rels)]);
        let mut archive = Archive::open(bytes, "DOCX", &limits()).unwrap();
        let rels = archive.read_rels("word/document.xml").unwrap();

        let link = rels.get("rId1").unwrap();
        assert!(link.external);
        assert_eq!(link.target, "https://example.com/a?b=1&c=2");
        assert_eq!(rels.get("rId2").unwrap().target, "word/styles.xml");
        assert!(
            rels.get("rId3").is_none(),
            "escaping target must be dropped"
        );
        assert_eq!(rels.of_type("styles").count(), 1);
        assert_eq!(
            rels.of_type("hyperlink")
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            ["rId1"]
        );
    }

    #[test]
    fn rels_nested_past_depth_limit_are_rejected() {
        let depth = 50;
        let xml = format!("{}{}", "<a>".repeat(depth), "</a>".repeat(depth));
        let bytes = test_zip::build(&[("word/_rels/document.xml.rels", xml.as_bytes())]);
        let limits = ParseLimits {
            max_nesting_depth: 10,
            ..limits()
        };
        let mut archive = Archive::open(bytes, "DOCX", &limits).unwrap();
        let err = archive.read_rels("word/document.xml").unwrap_err();
        assert!(err.to_string().contains("max_nesting_depth"), "{err}");
    }

    #[test]
    fn missing_rels_part_is_empty() {
        let bytes = test_zip::build(&[("word/document.xml", b"<d/>")]);
        let mut archive = Archive::open(bytes, "DOCX", &limits()).unwrap();
        assert!(
            archive
                .read_rels("word/document.xml")
                .unwrap()
                .get("rId1")
                .is_none()
        );
    }
}
