//! The Cha C compiler (chacc).

mod ast;
mod codegen;
mod constexpr;
mod error;
mod parse;
mod source;
mod tokenize;
mod types;
mod utils;

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use lexopt::Arg;

use crate::codegen::Codegen;
use crate::error::{Error, Result};
use crate::parse::Parser;
use crate::source::Source;
use crate::tokenize::Tokenizer;

fn help() {
    println!("Usage: chacc [options] <input>");
    println!();
    println!("Options:");
    println!("  -o/--output <path>  Write output to <path>");
    println!("  -###                Print subprocess commands");
    println!("  --pass-exit-codes   Exit with the highest error code from a phase");
    println!("  -h, --help          Show this help message");
}

/// The chacc compiler driver.
#[derive(Debug)]
pub struct Driver {
    highest_exit_code: Option<u8>,

    // Positional arguments
    input: PathBuf,

    // Public options
    output: PathBuf,
    hash_hash_hash: bool,
    pass_exit_codes: bool,

    // Hidden options
    pub cc1: bool,
}

impl Driver {
    /// Initialize the driver from the command-line arguments.
    pub fn from_cli() -> Result<Self, lexopt::Error> {
        let mut input = None;
        let mut output = None;
        let mut hash_hash_hash = false;
        let mut pass_exit_codes = false;
        let mut cc1 = false;

        let mut parser = lexopt::Parser::from_env();

        loop {
            if let Some(mut raw) = parser.try_raw_args() {
                match raw.peek().and_then(|arg| arg.to_str()) {
                    Some("-###") => {
                        raw.next();
                        hash_hash_hash = true;
                        continue;
                    },
                    Some("-pass-exit-codes") => {
                        raw.next();
                        pass_exit_codes = true;
                        continue;
                    },
                    Some("-cc1") => {
                        raw.next();
                        cc1 = true;
                        continue;
                    },
                    _ => {},
                }
            }

            let Some(arg) = parser.next()? else {
                break;
            };

            match arg {
                Arg::Short('o') | Arg::Long("output") => {
                    output = Some(PathBuf::from(parser.value()?));
                },
                Arg::Short('h') | Arg::Long("help") => {
                    help();
                    std::process::exit(0);
                },
                Arg::Value(path) => {
                    if input.replace(PathBuf::from(path)).is_some() {
                        return Err("multiple input files are not supported yet".into());
                    }
                },
                _ => return Err(arg.unexpected()),
            }
        }

        Ok(Self {
            input: input.ok_or("no input file")?,
            output: output.unwrap_or_else(|| PathBuf::from("a.out")),
            hash_hash_hash,
            pass_exit_codes,
            cc1,
            highest_exit_code: None,
        })
    }

    /// Run the driver to execute the compilation process.
    pub fn run(&mut self) -> Result<()> {
        if self.cc1 {
            self.run_cc1()?;
            return Ok(());
        }

        self.run_subprocess({
            let mut command = Command::new(std::env::current_exe()?);
            command.arg("-cc1");
            command.arg("-o");
            command.arg(&self.output);
            command.arg(&self.input);
            command
        })?;

        Ok(())
    }

    /// Get the highest exit code recorded, only if requested.
    pub fn code(&self) -> Option<ExitCode> {
        if !self.pass_exit_codes {
            return None;
        }
        self.highest_exit_code.map(ExitCode::from)
    }

    /// Run a subprocess command.
    fn run_subprocess(&mut self, mut command: Command) -> Result<()> {
        if self.hash_hash_hash {
            eprintln!("{command:?}");
        }

        let status = command.status()?;
        if status.success() {
            return Ok(());
        }

        if self.pass_exit_codes {
            self.highest_exit_code = self
                .highest_exit_code
                .max(status.code().map(|code| code as u8));
        }
        Err(Error::Terminate)
    }

    /// Run the core compilation logic (in cc1 mode).
    fn run_cc1(&self) -> Result<()> {
        let source = if self.input.as_os_str() == "-" {
            Source::from_stdin()?
        } else {
            Source::from_path(&self.input)?
        };

        let tokens = Tokenizer::new(&source).tokenize()?;
        let program = Parser::new(&source, tokens).parse_program()?;
        let codegen = Codegen::new(&source, &self.output)?;
        codegen.generate(program)?;

        Ok(())
    }
}
