//! Localized quota text (P-D-268): a map from locale tag to text, with English required. A tag
//! matches `^[a-z]{2,3}(-[A-Za-z0-9]{1,8})*$`, a map holds at most 32 tags and a text at most 2000
//! characters. A read serves the requested tag, else English, and names the tag it served; text
//! is never a key.
use std::collections::BTreeMap;

use super::record::RecordRefusal;

/// The locale every quota text must carry.
pub const ENGLISH: &str = "en";
/// The most locales one text map holds.
pub const MAX_LOCALES: usize = 32;
/// The most characters one localized text holds.
pub const MAX_TEXT_CHARS: usize = 2000;

/// Checks one localized text map: its size, every tag, every text's length, then a non-blank
/// English text.
///
/// # Errors
/// `RecordRefusal` `FIELD_TOO_LONG` (more than [`MAX_LOCALES`] tags, a text over
/// [`MAX_TEXT_CHARS`]), `VALIDATION` (a bad tag) or `QUOTA_DISPLAY_TEXT_MISSING` (no non-blank
/// `en`), on `field`.
pub fn check_localized(field: &str, text: &BTreeMap<String, String>) -> Result<(), RecordRefusal> {
    let refusal = |code: &'static str, detail: String| RecordRefusal {
        code,
        field: field.to_owned(),
        detail,
    };
    if text.len() > MAX_LOCALES {
        return Err(refusal(
            "FIELD_TOO_LONG",
            format!("at most {MAX_LOCALES} locales"),
        ));
    }
    if let Some(tag) = text.keys().find(|tag| !is_tag(tag)) {
        return Err(refusal(
            "VALIDATION",
            format!("{tag:?} is not a locale tag"),
        ));
    }
    if let Some(tag) = text
        .iter()
        .find_map(|(tag, text)| (text.chars().count() > MAX_TEXT_CHARS).then_some(tag))
    {
        return Err(refusal(
            "FIELD_TOO_LONG",
            format!("the {tag} text is longer than {MAX_TEXT_CHARS} characters"),
        ));
    }
    if text
        .get(ENGLISH)
        .is_none_or(|english| english.trim().is_empty())
    {
        return Err(refusal(
            "QUOTA_DISPLAY_TEXT_MISSING",
            "an English text is required".to_owned(),
        ));
    }
    Ok(())
}

/// The text for `requested`, else English, with the tag served. A map without English (never one
/// [`check_localized`] passed) serves its first tag.
#[must_use]
pub fn resolve<'a>(
    text: &'a BTreeMap<String, String>,
    requested: Option<&str>,
) -> (&'a str, &'a str) {
    requested
        .and_then(|tag| text.get_key_value(tag))
        .or_else(|| text.get_key_value(ENGLISH))
        .or_else(|| text.iter().next())
        .map_or(("", ENGLISH), |(tag, text)| (text.as_str(), tag.as_str()))
}

/// `^[a-z]{2,3}(-[A-Za-z0-9]{1,8})*$`.
fn is_tag(tag: &str) -> bool {
    let mut parts = tag.split('-');
    let language_ok = parts.next().is_some_and(|language| {
        (2..=3).contains(&language.len()) && language.bytes().all(|b| b.is_ascii_lowercase())
    });
    language_ok
        && parts.all(|part| {
            (1..=8).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_alphanumeric())
        })
}

#[cfg(test)]
#[path = "locale_tests.rs"]
mod locale_tests;
