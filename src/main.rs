use std::process::ExitCode;

fn main() -> ExitCode {
    match chacc::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        },
    }
}
