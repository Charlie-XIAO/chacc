//! The Cha C compiler (chacc).

mod ast;
mod cli;
mod codegen;
mod constexpr;
mod error;
mod parse;
mod source;
mod tokenize;
mod types;
mod utils;

use std::path::Path;
use std::process::{Command, ExitCode};

use tempfile::TempDir;

use crate::cli::Cli;
use crate::codegen::Codegen;
use crate::error::{Error, Result};
use crate::parse::Parser;
use crate::source::Source;
use crate::tokenize::Tokenizer;

/// The chacc compiler driver.
#[derive(Debug)]
struct Driver {
    cli: Cli,
    highest_exit_code: Option<u8>,
}

impl Driver {
    fn new(cli: Cli) -> Self {
        Self {
            cli,
            highest_exit_code: None,
        }
    }

    /// Run the driver to execute the compilation process.
    fn run(&mut self) -> Result<()> {
        if self.cli.cc1 {
            cc1(&self.cli.input, &self.cli.output)?;
            return Ok(());
        }

        if self.cli.compile_only {
            self.run_cc1(None, None)?;
            return Ok(());
        }

        // Create temporarily directory only if we are not keeping temp files
        let tmp = if self.cli.save_temps {
            None
        } else {
            Some(TempDir::with_prefix("chacc")?)
        };

        let asm_path = self
            .cli
            .temp_output_path("s", tmp.as_ref().map(TempDir::path));
        self.run_cc1(None, Some(&asm_path))?;
        self.run_assemble(Some(&asm_path), None)?;

        Ok(())
    }

    /// Get the highest exit code recorded, only if requested.
    fn code(&self) -> Option<ExitCode> {
        if !self.cli.pass_exit_codes {
            return None;
        }
        self.highest_exit_code.map(ExitCode::from)
    }

    /// Run a subprocess command.
    fn run_subprocess(&mut self, mut command: Command) -> Result<()> {
        if self.cli.print_subprocess_commands {
            eprint!("{}", command.get_program().to_string_lossy());
            for arg in command.get_args() {
                eprint!(" {}", arg.to_string_lossy());
            }
            eprintln!();
        }

        let status = command.status()?;
        if status.success() {
            return Ok(());
        }

        if self.cli.pass_exit_codes {
            self.highest_exit_code = self
                .highest_exit_code
                .max(status.code().map(|code| code as u8));
        }
        Err(Error::Terminate)
    }

    /// Run cc1 in a subprocess.
    ///
    /// The CLI input/output path will be used if not provided.
    fn run_cc1(&mut self, input: Option<&Path>, output: Option<&Path>) -> Result<()> {
        self.run_subprocess({
            let mut command = Command::new(std::env::current_exe()?);
            command.arg("-cc1");
            command.arg("-o");
            command.arg(output.unwrap_or(&self.cli.output));
            command.arg(input.unwrap_or(&self.cli.input));
            command
        })
    }

    /// Run the assembler in a subprocess.
    ///
    /// The CLI input/output path will be used if not provided.
    fn run_assemble(&mut self, input: Option<&Path>, output: Option<&Path>) -> Result<()> {
        self.run_subprocess({
            let mut command = Command::new(self.cli.resolve_tool("as"));
            command.arg("-c");
            command.arg(input.unwrap_or(&self.cli.input));
            command.arg("-o");
            command.arg(output.unwrap_or(&self.cli.output));
            command
        })
    }
}

/// Core compilation logic (cc1).
fn cc1(input: &Path, output: &Path) -> Result<()> {
    let source = Source::new(input)?;
    let tokens = Tokenizer::new(&source).tokenize()?;
    let program = Parser::new(&source, tokens).parse_program()?;
    let codegen = Codegen::new(&source, output)?;
    codegen.generate(program)?;

    Ok(())
}

fn main() -> ExitCode {
    let cli = match Cli::parse() {
        Ok(cli) => cli,
        Err(e) => {
            eprintln!("chacc: {e}");
            return ExitCode::FAILURE;
        },
    };

    let mut driver = Driver::new(cli);
    match driver.run() {
        Ok(()) => driver.code().unwrap_or(ExitCode::SUCCESS),
        Err(e) => {
            if e.is_terminate() {
                // Termination is not an error that needs to be reported
            } else if driver.cli.cc1 {
                eprintln!("cc1: {e}");
            } else {
                eprintln!("chacc: {e}");
            }
            driver.code().unwrap_or(ExitCode::FAILURE)
        },
    }
}
