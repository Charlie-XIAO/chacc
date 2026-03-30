//! Errors and diagnostics for chacc.

use smol_str::SmolStr;

/// The severity level of a diagnostic message.
#[derive(Clone, Copy, Debug)]
pub enum DiagnosticLevel {
    Warning,
    Error,
}

impl std::fmt::Display for DiagnosticLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Warning => write!(f, "warning"),
            Self::Error => write!(f, "error"),
        }
    }
}

/// A compiler diagnostic message.
#[derive(Debug, thiserror::Error)]
pub struct Diagnostic {
    pub level: DiagnosticLevel,
    pub source_name: SmolStr,
    pub source_content: SmolStr,
    pub message: SmolStr,
    pub line: usize,
    pub column: usize,
    pub line_start: usize,
    pub line_end: usize,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}:{}: {}: {}\n{}\n{}^",
            self.source_name,
            self.line,
            self.column,
            self.level,
            self.message,
            self.source_content[self.line_start..self.line_end].trim_end_matches(['\r', '\n']),
            " ".repeat(self.column.saturating_sub(1)),
        )
    }
}

/// The error type for chacc.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Diagnostic(#[from] Diagnostic),
}

/// Replaces [`std::result::Result`], using [`Error`] as the default error type.
pub type Result<T, E = Error> = std::result::Result<T, E>;
