//! Program source definition.

use std::io::Read;
use std::path::PathBuf;

use line_index::{LineIndex, TextSize};
use smol_str::{SmolStr, ToSmolStr};

use crate::error::{Diagnostic, DiagnosticLevel, Error, Result};

/// A source file.
#[derive(Debug)]
pub enum SourceFile {
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
    file: SourceFile,
    content: SmolStr,
    line_index: LineIndex,
}

impl Source {
    /// Construct a source file from a path.
    pub fn from_path(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let content =
            std::fs::read_to_string(&path).map_err(|e| Error::IoWithPath(path.clone(), e))?;
        Ok(Self::new(SourceFile::Path(path), content))
    }

    /// Construct a source file from stdin.
    pub fn from_stdin() -> Result<Self> {
        let mut content = String::new();
        std::io::stdin().read_to_string(&mut content)?;
        Ok(Self::new(SourceFile::Stdin, content))
    }

    fn new(file: SourceFile, content: impl Into<SmolStr>) -> Self {
        let content = content.into();
        let line_index = LineIndex::new(&content);
        Self {
            file,
            content,
            line_index,
        }
    }

    pub fn file(&self) -> &SourceFile {
        &self.file
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    /// Get the line and column number (1-based) of the given byte offset.
    pub fn line_col(&self, offset: usize) -> (u32, u32) {
        let line_col = self.line_index.line_col(text_size(offset));
        (line_col.line + 1, line_col.col + 1)
    }

    /// Return an error diagnostic at the given offset.
    pub fn error_at(&self, offset: usize, message: impl Into<SmolStr>) -> Error {
        self.diagnostic_at(offset, DiagnosticLevel::Error, message)
            .into()
    }

    /// Emit a warning message at the given byte offset.
    pub fn warn_at(&self, offset: usize, message: impl Into<SmolStr>) {
        eprintln!(
            "{}",
            self.diagnostic_at(offset, DiagnosticLevel::Warning, message)
        );
    }

    fn diagnostic_at(
        &self,
        offset: usize,
        level: DiagnosticLevel,
        message: impl Into<SmolStr>,
    ) -> Diagnostic {
        let line_col = self.line_index.line_col(text_size(offset));
        let range = self
            .line_index
            .line(line_col.line)
            .expect("invalid line index");

        let line_start = usize::from(range.start());
        let line_end = usize::from(range.end());

        Diagnostic {
            level,
            source_name: self.file.to_smolstr(),
            source_content: self.content.clone(),
            message: message.into(),
            line: (line_col.line as usize) + 1,
            column: (line_col.col as usize) + 1,
            line_start,
            line_end,
        }
    }
}

fn text_size(offset: usize) -> TextSize {
    TextSize::try_from(offset).expect("invalid byte offset")
}
