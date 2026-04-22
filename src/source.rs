//! Program source definition.

use std::{io::Read, path::Path};

use line_index::{LineIndex, TextSize};
use smol_str::{SmolStr, ToSmolStr};

use crate::error::{Diagnostic, DiagnosticLevel, Error, Result};

/// A C program source to be compiled.
#[derive(Debug)]
pub struct Source {
    name: SmolStr,
    content: SmolStr,
    line_index: LineIndex,
}

impl Source {
    /// Construct a source file from a path.
    ///
    /// If the path is "-", read from standard input.
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();

        let (content, name) = if path.as_os_str() == "-" {
            let mut content = String::new();
            std::io::stdin().read_to_string(&mut content)?;
            (content, "<stdin>".to_smolstr())
        } else {
            let content = std::fs::read_to_string(&path)
                .map_err(|e| Error::IoWithPath(path.to_path_buf(), e))?;
            (content, path.display().to_smolstr())
        };

        let line_index = LineIndex::new(&content);

        Ok(Self {
            name,
            content: content.into(),
            line_index,
        })
    }

    pub fn name(&self) -> SmolStr {
        self.name.clone()
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
        eprintln!(
            "{}",
            self.diagnostic_at(offset, DiagnosticLevel::Error, message)
        );
        Error::Terminate
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
            source_name: self.name.clone(),
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
