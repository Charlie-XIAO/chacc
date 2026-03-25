use chacc::Source;
use clap::Parser;

/// Command-line arguments for the compiler.
#[derive(Debug, Parser)]
struct Cli {
    #[arg(allow_hyphen_values = true)]
    input: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let source = if cli.input == "-" {
        Source::from_stdin()?
    } else {
        Source::from_path(cli.input)?
    };

    let assembly = chacc::compile(&source)?;
    println!("{assembly}");

    Ok(())
}
