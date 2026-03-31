//! The Cha C compiler (chacc).

mod ast;
mod codegen;
mod error;
mod parse;
mod source;
mod tokenize;
mod types;
mod utils;

use std::path::PathBuf;

use clap::Parser as _;

use crate::codegen::Codegen;
use crate::error::Result;
use crate::parse::Parser;
use crate::source::Source;
use crate::tokenize::Tokenizer;

/// The Cha C compiler (chacc).
#[derive(Debug, clap::Parser)]
struct Cli {
    /// The input file path, or "-" for stdin.
    input: PathBuf,
    /// The output file path.
    #[arg(short, default_value = "a.out")]
    output: PathBuf,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();

    let source = if cli.input.as_os_str() == "-" {
        Source::from_stdin()?
    } else {
        Source::from_path(cli.input)?
    };

    let tokens = Tokenizer::new(&source).tokenize()?;
    let program = Parser::new(&source, tokens).parse_program()?;
    let codegen = Codegen::new(&source, &cli.output)?;
    codegen.generate(program)?;

    Ok(())
}
