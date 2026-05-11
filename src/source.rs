//! Program source definition.

use std::io::Read;
use std::path::PathBuf;

use line_index::{LineCol, LineIndex, TextSize};
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
    /// The normalized source content for tokenization.
    pub content: SmolStr,
    /// The original source text.
    original: SmolStr,
    /// The line index on the original source.
    line_index: LineIndex,
    /// The mapping from logical offsets to physical offsets.
    ///
    /// Each entry is a pair of logical offset and cumulative shift, only
    /// where physical and logical offsets differ. All logical offsets in the
    /// same range `[offset, next_offset)` share the same cumulative shift. The
    /// vector is sorted by logical offsets.
    shifts: Vec<(usize, usize)>,
}

impl Source {
    fn new(id: usize, path: PathBuf, name: SmolStr, original: SmolStr) -> Self {
        let bytes = original.as_bytes();
        let mut content = String::new();
        let mut shifts: Vec<(usize, usize)> = Vec::new();

        let mut i = 0;
        let mut start = 0;
        let mut cum_shift = 0;

        while i < bytes.len() {
            let removed_len = match &bytes[i..] {
                [b'\\', b'\n', ..] => 2,
                [b'\\', b'\r', b'\n', ..] => 3,
                _ => {
                    i += 1;
                    continue;
                },
            };

            if content.is_empty() {
                content.reserve(original.len());
            }
            content.push_str(&original[start..i]);
            cum_shift += removed_len;

            let logical_offset = content.len();
            if let Some((offset, shift)) = shifts.last_mut()
                && *offset == logical_offset
            {
                *shift = cum_shift;
            } else {
                shifts.push((logical_offset, cum_shift));
            }

            i += removed_len;
            start = i;
        }

        let content = if shifts.is_empty() {
            original.clone()
        } else {
            content.push_str(&original[start..]);
            content.into()
        };

        let line_index = LineIndex::new(&original);

        Self {
            id,
            path,
            name,
            content,
            original,
            line_index,
            shifts,
        }
    }

    /// Return the [`LineCol`] in original source of the given logical offset.
    fn line_col(&self, mut offset: usize) -> LineCol {
        let idx = self.shifts.partition_point(|shift| shift.0 <= offset);
        if idx > 0 {
            offset += self.shifts[idx - 1].1;
        }
        let offset = TextSize::try_from(offset).expect("invalid byte offset");
        self.line_index
            .try_line_col(offset)
            .expect("invalid byte offset")
    }

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
        let line_col = self.line_col(offset);
        let range = self
            .line_index
            .line(line_col.line)
            .expect("invalid line index");

        let line_no = line_col.line as usize;
        let col_no = line_col.col as usize;

        let line_start = usize::from(range.start());
        let line_end = usize::from(range.end());
        let line = self.original[line_start..line_end].trim_end_matches(['\r', '\n']);
        let span_len = len.min(line.len().saturating_sub(col_no));

        Diagnostic {
            level,
            message: message.into(),
            file: &self.name,
            line,
            line_no: line_no + 1,
            col_no: col_no + 1,
            span_len,
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

        let (original, name) = if path.as_os_str() == "-" {
            let mut content = String::new();
            std::io::stdin().read_to_string(&mut content)?;
            (content, "<stdin>".to_smolstr())
        } else {
            let content =
                std::fs::read_to_string(&path).map_err(|e| Error::IoWithPath(path.clone(), e))?;
            (content, path.display().to_smolstr())
        };

        let source = Source::new(self.0.len(), path, name, original.into());
        self.0.push(source);
        Ok(self.0.last().unwrap())
    }

    /// Push a new virtual source file with the given content.
    ///
    /// Returns a reference to the newly created source.
    pub fn push_virtual(&mut self, content: SmolStr) -> &Source {
        let id = self.0.len();
        let source = Source::new(
            id,
            PathBuf::new(),
            format_smolstr!("<virtual-{id}>"),
            content,
        );
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
        let line_col = source.line_col(span.offset);
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
