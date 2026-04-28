//! Errors and diagnostics for chacc.

use std::path::PathBuf;

/// The severity level of a diagnostic message.
#[derive(Clone, Copy, Debug, strum::Display)]
#[strum(serialize_all = "lowercase")]
pub enum DiagnosticLevel {
    Warning,
    Error,
}

/// A compiler diagnostic message.
#[derive(Debug)]
pub struct Diagnostic<'a> {
    pub level: DiagnosticLevel,
    pub message: String,
    pub file: &'a str,
    pub line: &'a str,
    pub line_no: usize,
    pub col_no: usize,
    pub span_len: usize,
}

impl<'a> std::fmt::Display for Diagnostic<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}:{}: {}: {}\n{}\n{}^{}",
            self.file,
            self.line_no,
            self.col_no,
            self.level,
            self.message,
            self.line,
            " ".repeat(self.col_no.saturating_sub(1)),
            "~".repeat(self.span_len.saturating_sub(1)),
        )
    }
}

/// The error type for chacc.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("compilation terminated")]
    Terminate,
    #[error("{0} (run with -h/--help for usage)")]
    Cli(#[from] lexopt::Error),
    #[error("fatal error: {0}")]
    Io(#[from] std::io::Error),
    #[error("fatal error: {0}: {1}")]
    IoWithPath(PathBuf, std::io::Error),
    #[error("cannot resolve host compiler toolchain: {0}")]
    HostccNotFound(String),
    #[error("host compiler cannot resolve '{0}'")]
    HostccResolutionFailed(&'static str),
}

impl Error {
    /// Returns whether this is a termination.
    pub fn is_terminate(&self) -> bool {
        matches!(self, Self::Terminate)
    }
}

/// Replaces [`std::result::Result`], using [`Error`] as the default error type.
pub type Result<T, E = Error> = std::result::Result<T, E>;
