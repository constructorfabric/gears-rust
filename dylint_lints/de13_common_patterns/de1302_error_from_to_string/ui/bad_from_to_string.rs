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

#[derive(Debug)]
struct AppError(String);

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<DatabaseError> for AppError {
    fn from(e: DatabaseError) -> Self {
        // Should trigger DE1302 - to_string loses error chain
        AppError(e.to_string())
    }
}

fn main() {}
