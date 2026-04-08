// Created: 2026-03-13 by Constructor Tech
// Updated: 2026-03-13 by Constructor Tech
#![allow(dead_code)]

use std::fmt;

#[derive(Debug)]
struct DatabaseError(String);

impl fmt::Display for DatabaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for DatabaseError {}

// Good - store the source error directly (preserves chain)
#[derive(Debug)]
enum AppError {
    Database(DatabaseError),
    Other(String),
}

impl From<DatabaseError> for AppError {
    fn from(e: DatabaseError) -> Self {
        AppError::Database(e)  // error chain preserved
    }
}

// Good - From<String> for error types does not involve an Error source
#[derive(Debug)]
struct ParseError(String);

impl From<String> for ParseError {
    fn from(s: String) -> Self {
        ParseError(s.to_string())  // not From<XxxError>, not flagged
    }
}

// Good - to_string() in a non-From method is fine
impl AppError {
    fn message(&self) -> String {
        format!("{:?}", self)
    }
}

// Good - to_string() on a non-Error receiver inside From<Error> body is NOT flagged.
// The receiver "database layer" is &str, which does not implement Error.
#[derive(Debug)]
struct ContextError {
    source: DatabaseError,
    context: String,
}

impl fmt::Display for ContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.context, self.source)
    }
}

impl std::error::Error for ContextError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

impl From<DatabaseError> for ContextError {
    fn from(e: DatabaseError) -> Self {
        ContextError {
            source: e,
            context: "database layer".to_string(), // receiver is &str, not Error — not flagged
        }
    }
}

fn main() {}
