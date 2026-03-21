use std::process::ExitCode;

use clap::Parser;

/// Command-line arguments for the compiler.
#[derive(Debug, Parser)]
struct Cli {
    #[arg(allow_hyphen_values = true)]
    input: String,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match chacc::compile_expression_program(&cli.input) {
        Ok(assembly) => {
            print!("{assembly}");
            ExitCode::SUCCESS
        },
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        },
    }
}
