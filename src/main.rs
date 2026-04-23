//! The Cha C compiler (chacc).

use std::process::ExitCode;

use crate::cli::Cli;
use crate::driver::Driver;

mod ast;
mod cli;
mod codegen;
mod constexpr;
mod driver;
mod error;
mod parse;
mod source;
mod tokenize;
mod types;
mod utils;

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
            } else if driver.cc1() {
                eprintln!("cc1: {e}");
            } else {
                eprintln!("chacc: {e}");
            }
            driver.code().unwrap_or(ExitCode::FAILURE)
        },
    }
}
