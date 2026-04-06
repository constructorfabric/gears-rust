//! Error catalog support (`ErrDef` for use with `declare_errors`! macro)

use crate::problem::Problem;
use http::StatusCode;

/// Static error definition from catalog
#[derive(Debug, Clone, Copy)]
pub struct ErrDef {
    pub status: u16,
    pub title: &'static str,
    pub code: &'static str,
    pub type_url: &'static str,
}

impl ErrDef {
    /// Convert this error definition into a Problem with the given detail
    #[inline]
    pub fn as_problem(&self, detail: impl Into<String>) -> Problem {
        // Convert u16 to StatusCode, using INTERNAL_SERVER_ERROR as fallback for invalid codes
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        Problem::new(status, self.title, detail.into())
            .with_code(self.code)
            .with_type(self.type_url)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "catalog_tests.rs"]
mod tests;
