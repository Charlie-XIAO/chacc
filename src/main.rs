use std::process::ExitCode;

use chacc::Driver;

fn main() -> ExitCode {
    let mut driver = match Driver::from_cli() {
        Ok(cli) => cli,
        Err(e) => {
            eprintln!("chacc: {e}");
            eprintln!("chacc: run with -h/--help for usage");
            return ExitCode::FAILURE;
        },
    };

    match driver.run() {
        Ok(()) => driver.code().unwrap_or(ExitCode::SUCCESS),
        Err(e) => {
            if e.is_diagnostic() {
                eprintln!("{e}")
            } else if driver.cc1 {
                eprintln!("cc1: {e}");
            } else {
                eprintln!("chacc: {e}");
            }
            driver.code().unwrap_or(ExitCode::FAILURE)
        },
    }
}
