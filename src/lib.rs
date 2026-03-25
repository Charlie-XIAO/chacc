//! The Cha C compiler (chacc).

mod ast;
mod codegen;
mod parse;
mod source;
mod tokenize;
mod types;

use std::path::Path;

use codegen::Codegen;
use parse::Cursor;
pub use source::Source;
use tokenize::Tokenizer;

/// Compile a source into x86-64 assembly.
pub fn compile(source: &Source, output: &Path) -> Result<(), String> {
    let tokens = Tokenizer::new(source).tokenize()?;
    let program = Cursor::new(source, tokens).parse_program()?;
    Codegen::new(source, output).generate(program)
}
