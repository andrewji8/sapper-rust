//! Unified user-facing error. `sapper: {message}` is printed to stderr and
//! the CLI exits 1, mirroring the Python `FGError` contract.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct SapperError(pub String);

impl SapperError {
    pub fn new(msg: impl Into<String>) -> Self {
        SapperError(msg.into())
    }
}

impl fmt::Display for SapperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SapperError {}

impl From<String> for SapperError {
    fn from(s: String) -> Self {
        SapperError(s)
    }
}

impl From<&str> for SapperError {
    fn from(s: &str) -> Self {
        SapperError(s.to_string())
    }
}

impl From<std::io::Error> for SapperError {
    fn from(e: std::io::Error) -> Self {
        SapperError(e.to_string())
    }
}

impl From<serde_json::Error> for SapperError {
    fn from(e: serde_json::Error) -> Self {
        SapperError(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, SapperError>;
