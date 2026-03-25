//! Program source and diagnostics.

use std::io::Read;
use std::path::PathBuf;

use line_index::{LineIndex, TextSize};

/// A source file.
#[derive(Debug)]
enum SourceFile {
    Stdin,
    Path(PathBuf),
}

impl std::fmt::Display for SourceFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceFile::Stdin => write!(f, "<stdin>"),
            SourceFile::Path(path) => write!(f, "{}", path.display()),
        }
    }
}

/// A C program source to be compiled.
#[derive(Debug)]
pub struct Source {
    source: SourceFile,
    content: String,
    line_index: LineIndex,
}

impl Source {
    /// Construct a source file from a path.
    pub fn from_path(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let content = std::fs::read_to_string(&path)?;
        let line_index = LineIndex::new(&content);

        Ok(Self {
            source: SourceFile::Path(path),
            content,
            line_index,
        })
    }

    /// Construct a source file from stdin.
    pub fn from_stdin() -> std::io::Result<Self> {
        let mut content = String::new();
        std::io::stdin().read_to_string(&mut content)?;
        let line_index = LineIndex::new(&content);

        Ok(Self {
            source: SourceFile::Stdin,
            content,
            line_index,
        })
    }

    /// Get a reference to the source content.
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Format an error message rooted at the given byte offset.
    pub fn error_at(&self, offset: usize, message: &str) -> String {
        self.format_diagnostic_at(offset, "ERROR", message)
    }

    /// Emit a warning message rooted at the given byte offset.
    pub fn emit_warning_at(&self, offset: usize, message: &str) {
        eprintln!("{}", self.format_diagnostic_at(offset, "WARNING", message));
    }

    fn format_diagnostic_at(&self, offset: usize, level: &str, message: &str) -> String {
        let offset = TextSize::try_from(offset).expect("invalid byte offset");
        let line_col = self.line_index.line_col(offset);
        let range = self
            .line_index
            .line(line_col.line)
            .expect("invalid line index");

        let start = usize::from(range.start());
        let end = usize::from(range.end());
        let line = self.content[start..end].trim_end_matches(['\r', '\n']);

        format!(
            "{}:{}: {}: {}\n{}\n{}^",
            self.source,
            line_col.line + 1,
            level,
            message,
            line,
            " ".repeat(line_col.col as _)
        )
    }
}
