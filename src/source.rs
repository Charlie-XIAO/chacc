//! Program source definition.

use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::rc::Rc;

use line_index::{LineCol, LineIndex, TextSize};
use rustc_hash::FxHashMap;
use smol_str::{SmolStr, ToSmolStr, format_smolstr};
use unicode_width::UnicodeWidthStr;

use crate::error::{Diagnostic, DiagnosticLevel, Error, Result};

/// An identifier for a [`Source`] in a [`SourceMap`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SourceId(usize);

impl SourceId {
    const FAKE: Self = Self(usize::MAX);
}

/// A span in a [`Source`].
#[derive(Clone, Copy, Debug)]
pub struct SourceSpan {
    pub id: SourceId,
    pub offset: usize,
    pub len: usize,
}

impl SourceSpan {
    /// Return a fake invalid source span.
    pub fn fake() -> Self {
        Self {
            id: SourceId::FAKE,
            offset: 0,
            len: 0,
        }
    }

    /// Return a zero-length span pointing right after this span.
    pub fn end(self) -> Self {
        Self {
            id: self.id,
            offset: self.offset + self.len,
            len: 0,
        }
    }

    /// Return a subspan relative to the start of this span.
    pub fn subspan(self, offset: usize, len: usize) -> Self {
        Self {
            id: self.id,
            offset: self.offset + offset,
            len,
        }
    }
}

/// A resolved logical source location.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceLoc<'a> {
    /// The file name of the source.
    pub file: &'a str,
    /// 1-based line number.
    pub line: u32,
    /// 1-based column number.
    pub col: u32,
}

/// Shared, immutable physical buffer of a source file or virtual text.
#[derive(Debug)]
struct SourceBuffer {
    /// The original (physical) source text.
    original: SmolStr,
    /// The normalized (logical) source content for tokenization.
    content: SmolStr,
    /// The line index on the original source.
    line_index: LineIndex,
    /// The mapping from logical offsets to physical offsets.
    ///
    /// Each entry is a pair of logical offset and cumulative shift, only
    /// where physical and logical offsets differ. All logical offsets in the
    /// same range `[offset, next_offset)` share the same cumulative shift. The
    /// vector is sorted by logical offsets.
    shifts: Vec<(usize, usize)>,
    /// The last modification time in Unix seconds, if available.
    mtime: Option<i64>,
}

impl SourceBuffer {
    fn new(original: SmolStr, mtime: Option<i64>) -> Self {
        let bytes = original.as_bytes();
        let mut content = String::new();
        let mut shifts: Vec<(usize, usize)> = Vec::new();

        let mut i = 0;
        let mut start = 0;
        let mut cum_shift = 0;
        let mut edited = false;

        enum Edit {
            ReplaceCrLf,
            ReplaceCr,
            Remove(usize),
        }

        // Skip UTF-8 BOM
        if original.starts_with('\u{feff}') {
            content.reserve(original.len());
            edited = true;
            i += 3;
            start = i;
            cum_shift += 3;
            shifts.push((0, cum_shift));
        }

        while i < bytes.len() {
            let edit = match &bytes[i..] {
                [b'\\', b'\n', ..] => Edit::Remove(2),
                [b'\\', b'\r', b'\n', ..] => Edit::Remove(3),
                [b'\\', b'\r', ..] => Edit::Remove(2),
                [b'\r', b'\n', ..] => Edit::ReplaceCrLf,
                [b'\r', ..] => Edit::ReplaceCr,
                _ => {
                    i += 1;
                    continue;
                },
            };

            if !edited {
                content.reserve(original.len());
            }
            edited = true;
            content.push_str(&original[start..i]);

            match edit {
                Edit::ReplaceCrLf => {
                    content.push('\n');
                    cum_shift += 1;
                    i += 2;
                },
                Edit::ReplaceCr => {
                    content.push('\n');
                    i += 1;
                },
                Edit::Remove(removed_len) => {
                    cum_shift += removed_len;
                    i += removed_len;
                },
            }

            if cum_shift != 0 {
                let logical_offset = content.len();
                if let Some((offset, shift)) = shifts.last_mut()
                    && *offset == logical_offset
                {
                    *shift = cum_shift;
                } else {
                    shifts.push((logical_offset, cum_shift));
                }
            }

            start = i;
        }

        let content = if !edited {
            original.clone()
        } else {
            content.push_str(&original[start..]);
            content.into()
        };

        let line_index = LineIndex::new(&original);

        Self {
            original,
            content,
            line_index,
            shifts,
            mtime,
        }
    }

    /// Return the [`LineCol`] in original source of the given logical offset.
    fn physical_line_col(&self, mut offset: usize) -> LineCol {
        let idx = self.shifts.partition_point(|shift| shift.0 <= offset);
        if idx > 0 {
            offset += self.shifts[idx - 1].1;
        }
        let offset = TextSize::try_from(offset).expect("invalid byte offset");
        self.line_index
            .try_line_col(offset)
            .expect("invalid byte offset")
    }

    fn diagnostic<'a>(
        &'a self,
        line_col: LineCol,
        len: usize,
        level: DiagnosticLevel,
        message: impl Into<String>,
        file: &'a str,
        line_no: u32,
    ) -> Diagnostic<'a> {
        let range = self
            .line_index
            .line(line_col.line)
            .expect("invalid line index");

        let col_no = line_col.col as usize;

        let line_start = usize::from(range.start());
        let line_end = usize::from(range.end());
        let line = self.original[line_start..line_end].trim_end_matches(['\r', '\n']);

        let span_start = col_no.min(line.len());
        let col_no = line[..span_start].width();

        let mut span_end = (span_start + len).min(line.len());
        while span_end < line.len() && !line.is_char_boundary(span_end) {
            span_end += 1; // Expand to valid UTF-8 char boundary
        }
        let span_len = line[span_start..span_end].width().max(1);

        Diagnostic {
            level,
            message: message.into(),
            file,
            line,
            line_no,
            col_no: col_no + 1,
            span_len,
        }
    }
}

/// A C program source unit.
///
/// This could be a source file, an `#include` occurrence, or a virtual buffer.
#[derive(Debug)]
pub struct Source {
    pub id: SourceId,
    /// The path if this is a real source file.
    pub path: Option<PathBuf>,
    /// The display name of this source.
    pub name: SmolStr,
    /// The shared immutable buffer for this source.
    buffer: Rc<SourceBuffer>,
}

impl Source {
    /// Return the normalized (logical) source content for tokenization.
    pub fn content(&self) -> &str {
        &self.buffer.content
    }

    /// Return the last modification time in Unix seconds, if available.
    pub fn mtime(&self) -> Option<i64> {
        self.buffer.mtime
    }

    /// Return the [`LineCol`] in original source of the given logical offset.
    fn physical_line_col(&self, offset: usize) -> LineCol {
        self.buffer.physical_line_col(offset)
    }

    /// Emit an error at the given span and return an error.
    pub fn error(&self, offset: usize, len: usize, message: impl Into<String>) -> Error {
        let line_col = self.physical_line_col(offset);
        eprintln!(
            "{}",
            self.buffer.diagnostic(
                line_col,
                len,
                DiagnosticLevel::Error,
                message,
                &self.name,
                line_col.line + 1,
            )
        );
        Error::Terminate
    }
}

/// A `#line` override entry within a [`Source`].
#[derive(Debug)]
struct LineEntry {
    /// 0-based physical line where this override takes effect.
    effective_line: u32,
    /// 1-based logical line number at [`LineEntry::effective_line`].
    logical_line: u32,
    /// The logical filename from [`LineEntry::effective_line`] onward.
    filename: SmolStr,
}

/// Logical line overrides keyed by [`SourceId`].
#[derive(Debug, Default)]
struct LineTable(FxHashMap<SourceId, Vec<LineEntry>>);

impl LineTable {
    /// Resolve the logical filename and 1-based line number.
    fn resolve<'a>(&'a self, source: &'a Source, physical_line: u32) -> (&'a str, u32) {
        let Some(entries) = self.0.get(&source.id) else {
            return (&source.name, physical_line + 1);
        };
        let idx = entries.partition_point(|e| e.effective_line <= physical_line);
        let Some(entry) = idx.checked_sub(1).map(|i| &entries[i]) else {
            return (&source.name, physical_line + 1);
        };
        let logical_line = entry
            .logical_line
            .saturating_add(physical_line - entry.effective_line);
        (&entry.filename, logical_line)
    }

    /// Record a `#line` override at `effective_line`.
    fn add(
        &mut self,
        source: &Source,
        effective_line: u32,
        logical_line: u32,
        filename: Option<SmolStr>,
    ) {
        let entries = self.0.entry(source.id).or_default();
        let idx = entries.partition_point(|e| e.effective_line <= effective_line);
        let filename = filename.unwrap_or_else(|| {
            idx.checked_sub(1)
                .map_or_else(|| source.name.clone(), |i| entries[i].filename.clone())
        });
        let entry = LineEntry {
            effective_line,
            logical_line,
            filename,
        };
        if idx > 0 && entries[idx - 1].effective_line == effective_line {
            entries[idx - 1] = entry;
        } else {
            entries.insert(idx, entry);
        }
    }
}

/// The collection of sources known to the compiler.
#[derive(Debug, Default)]
pub struct SourceMap {
    sources: Vec<Source>,
    buffer_cache: FxHashMap<PathBuf, Rc<SourceBuffer>>,
    line_table: LineTable,
}

impl SourceMap {
    /// Push a new source file by its path.
    ///
    /// Returns a reference to the newly created source.
    pub fn push(&mut self, path: impl Into<PathBuf>) -> Result<&Source> {
        let path = path.into();

        let (buffer, path, name) = if path.as_os_str() == "-" {
            let mut content = String::new();
            std::io::stdin().read_to_string(&mut content)?;
            (
                Rc::new(SourceBuffer::new(content.into(), None)),
                None,
                "<stdin>".to_smolstr(),
            )
        } else {
            let key = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            let buffer = if let Some(cached) = self.buffer_cache.get(&key) {
                cached.clone()
            } else {
                let mut file = File::open(&path).map_err(|e| Error::IoWithPath(path.clone(), e))?;
                let mtime = file.metadata().ok().map(|meta| meta.mtime());
                let mut content = String::new();
                file.read_to_string(&mut content)
                    .map_err(|e| Error::IoWithPath(path.clone(), e))?;
                let buffer = Rc::new(SourceBuffer::new(content.into(), mtime));
                self.buffer_cache.insert(key, buffer.clone());
                buffer
            };
            let name = path.display().to_smolstr();
            (buffer, Some(path), name)
        };

        let id = SourceId(self.sources.len());
        self.sources.push(Source {
            id,
            path,
            name,
            buffer,
        });
        Ok(self.sources.last().unwrap())
    }

    /// Push a new virtual source file with the given content.
    ///
    /// If the name is not provided, a default virtual name will be generated
    /// uniquely. Returns a reference to the newly created source.
    pub fn push_virtual(&mut self, content: SmolStr, name: Option<SmolStr>) -> &Source {
        let id = SourceId(self.sources.len());
        let name = name.unwrap_or_else(|| format_smolstr!("<virtual:{}>", id.0));
        let buffer = Rc::new(SourceBuffer::new(content, None));
        self.sources.push(Source {
            id,
            path: None,
            name,
            buffer,
        });
        self.sources.last().unwrap()
    }

    /// Return the source corresponding to the given ID.
    pub fn get(&self, id: SourceId) -> &Source {
        debug_assert!(id != SourceId::FAKE, "fake source id leaked here");
        self.sources.get(id.0).expect("invalid source id")
    }

    /// Get the text corresponding to the given span.
    pub fn text(&self, span: SourceSpan) -> &str {
        self.get(span.id)
            .content()
            .get(span.offset..span.offset + span.len)
            .expect("span out of range")
    }

    /// Resolve the logical source location of the given span.
    pub fn loc(&self, span: SourceSpan) -> SourceLoc<'_> {
        let source = self.get(span.id);
        let line_col = source.physical_line_col(span.offset);
        self.loc_at(source, line_col)
    }

    fn loc_at<'a>(&'a self, source: &'a Source, line_col: LineCol) -> SourceLoc<'a> {
        let (file, line) = self.line_table.resolve(source, line_col.line);
        SourceLoc {
            file,
            line,
            col: line_col.col + 1,
        }
    }

    /// Add a `#line` marker taking effect after `directive_end`.
    pub fn add_line_marker(
        &mut self,
        directive_end: SourceSpan,
        logical_line: u32,
        filename: Option<SmolStr>,
    ) {
        debug_assert!(
            directive_end.id != SourceId::FAKE,
            "fake source id leaked here"
        );
        let source = self
            .sources
            .get(directive_end.id.0)
            .expect("invalid source id");
        let effective_line = source.physical_line_col(directive_end.end().offset).line + 1;
        self.line_table
            .add(source, effective_line, logical_line, filename);
    }

    /// Emit an error at the given span and return an error.
    pub fn error(&self, span: SourceSpan, message: impl Into<String>) -> Error {
        eprintln!("{}", self.diagnostic(span, DiagnosticLevel::Error, message));
        Error::Terminate
    }

    /// Emit a warning at the given span.
    pub fn warn(&self, span: SourceSpan, message: impl Into<String>) {
        eprintln!(
            "{}",
            self.diagnostic(span, DiagnosticLevel::Warning, message)
        );
    }

    fn diagnostic(
        &self,
        span: SourceSpan,
        level: DiagnosticLevel,
        message: impl Into<String>,
    ) -> Diagnostic<'_> {
        let source = self.get(span.id);
        let line_col = source.physical_line_col(span.offset);
        let loc = self.loc_at(source, line_col);
        source
            .buffer
            .diagnostic(line_col, span.len, level, message, loc.file, loc.line)
    }
}
