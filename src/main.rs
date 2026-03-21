use std::process::ExitCode;

use clap::Parser;

/// Command-line arguments for the compiler.
#[derive(Debug, Parser)]
struct Cli {
    #[arg(allow_hyphen_values = true)]
    input: String,
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse_from(std::env::args_os()) {
        Ok(cli) => cli,
        Err(err) => err.exit(),
    };

    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// Compile the input expression and print assembly.
fn run(cli: Cli) -> Result<(), String> {
    let assembly = chacc::compile_expression_program(&cli.input)?;
    print!("{assembly}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::Cli;

    #[test]
    fn parses_the_single_input_argument() {
        let cli = Cli::try_parse_from(["chacc", "12 + 34"]).unwrap();
        assert_eq!(cli.input, "12 + 34");
    }

    #[test]
    fn parses_hyphen_prefixed_input() {
        let cli = Cli::try_parse_from(["chacc", "-10+20"]).unwrap();
        assert_eq!(cli.input, "-10+20");
    }

    #[test]
    fn rejects_the_wrong_number_of_arguments() {
        let error = Cli::try_parse_from(["chacc"]).unwrap_err();
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }
}
