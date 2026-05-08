//! Program source definition.

use std::io::Read;
use std::path::PathBuf;

use line_index::{LineIndex, TextSize};
use smol_str::{SmolStr, ToSmolStr, format_smolstr};

use crate::error::{Diagnostic, DiagnosticLevel, Error, Result};

/// A span in a [`Source`].
#[derive(Clone, Copy, Debug)]
pub struct SourceSpan {
    /// The source ID, corresponding to [`Source::id`].
    pub id: usize,
    pub offset: usize,
    pub len: usize,
}

/// A C program source to be compiled.
#[derive(Debug)]
pub struct Source {
    pub id: usize,
    pub path: PathBuf,
    pub name: SmolStr,
    pub content: SmolStr,
    line_index: LineIndex,
}

impl Source {
    /// Emit an error at the given span and return an error.
    pub fn error(&self, offset: usize, len: usize, message: impl Into<String>) -> Error {
        eprintln!(
            "{}",
            self.diagnostic(offset, len, DiagnosticLevel::Error, message)
        );
        Error::Terminate
    }

    /// Emit a warning at the given span.
    pub fn warn(&self, offset: usize, len: usize, message: impl Into<String>) {
        eprintln!(
            "{}",
            self.diagnostic(offset, len, DiagnosticLevel::Warning, message)
        );
    }

    fn diagnostic(
        &self,
        offset: usize,
        len: usize,
        level: DiagnosticLevel,
        message: impl Into<String>,
    ) -> Diagnostic<'_> {
        let line_col = self.line_index.line_col(text_size(offset));
        let range = self
            .line_index
            .line(line_col.line)
            .expect("invalid line index");

        let line_start = usize::from(range.start());
        let line_end = usize::from(range.end());
        let line = self.content[line_start..line_end].trim_end_matches(['\r', '\n']);

        Diagnostic {
            level,
            message: message.into(),
            file: &self.name,
            line,
            line_no: (line_col.line as usize) + 1,
            col_no: (line_col.col as usize) + 1,
            span_len: len,
        }
    }
}

/// The collection of source files known to the compiler.
#[derive(Debug, Default)]
pub struct SourceMap(Vec<Source>);

impl SourceMap {
    /// Push a new source file by its path.
    ///
    /// Returns a reference to the newly created source.
    pub fn push(&mut self, path: impl Into<PathBuf>) -> Result<&Source> {
        let path = path.into();

        let (content, name) = if path.as_os_str() == "-" {
            let mut content = String::new();
            std::io::stdin().read_to_string(&mut content)?;
            (content, "<stdin>".to_smolstr())
        } else {
            let content =
                std::fs::read_to_string(&path).map_err(|e| Error::IoWithPath(path.clone(), e))?;
            (content, path.display().to_smolstr())
        };

        let line_index = LineIndex::new(&content);

        let source = Source {
            id: self.0.len(),
            path,
            name,
            content: content.into(),
            line_index,
        };
        self.0.push(source);
        Ok(self.0.last().unwrap())
    }

    /// Push a new virtual source file with the given content.
    ///
    /// Returns a reference to the newly created source.
    pub fn push_virtual(&mut self, content: SmolStr) -> &Source {
        let line_index = LineIndex::new(&content);

        let id = self.0.len();
        let source = Source {
            id,
            path: PathBuf::new(),
            name: format_smolstr!("<virtual-{id}>"),
            content,
            line_index,
        };
        self.0.push(source);
        self.0.last().unwrap()
    }

    /// Return the source corresponding to the given ID.
    pub fn get(&self, id: usize) -> &Source {
        self.0.get(id).expect("invalid source id")
    }

    /// Iterate over all sources.
    pub fn iter(&self) -> impl Iterator<Item = &Source> {
        self.0.iter()
    }

    /// Get the text corresponding to the given span.
    pub fn text(&self, span: SourceSpan) -> &str {
        self.get(span.id)
            .content
            .get(span.offset..span.offset + span.len)
            .expect("span out of range")
    }

    /// Get the file/line/column numbers (1-based) of the given span.
    pub fn file_line_col(&self, span: SourceSpan) -> (usize, u32, u32) {
        let source = self.get(span.id);
        let line_col = source.line_index.line_col(text_size(span.offset));
        (span.id + 1, line_col.line + 1, line_col.col + 1)
    }

    /// Emit an error at the given span and return an error.
    pub fn error(&self, span: SourceSpan, message: impl Into<String>) -> Error {
        self.get(span.id).error(span.offset, span.len, message)
    }

    /// Emit a warning at the given span.
    pub fn warn(&self, span: SourceSpan, message: impl Into<String>) {
        self.get(span.id).warn(span.offset, span.len, message);
    }
}

fn text_size(offset: usize) -> TextSize {
    TextSize::try_from(offset).expect("invalid byte offset")
}
