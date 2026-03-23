//! The Cha C compiler (chacc).

mod ast;
mod codegen;
mod parse;
mod tokenize;
mod types;

use codegen::codegen_program;
use parse::Cursor;
use tokenize::tokenize;

/// Compile the input program into x86-64 assembly.
pub fn compile_expression_program(input: &str) -> Result<String, String> {
    let tokens = tokenize(input)?;
    let mut parser = Cursor::new(input, tokens);
    let program = parser.parse_program()?;
    codegen_program(input, program)
}
