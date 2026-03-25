use std::path::PathBuf;

use chacc::Source;
use clap::Parser;

/// The chacc C compiler.
#[derive(Debug, Parser)]
struct Cli {
    /// The input file path, or "-" for stdin.
    input: PathBuf,
    /// The output file path.
    #[arg(short, default_value = "a.out")]
    output: PathBuf,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let source = if cli.input.as_os_str() == "-" {
        Source::from_stdin()?
    } else {
        Source::from_path(cli.input)?
    };

    chacc::compile(&source, &cli.output)?;
    Ok(())
}
