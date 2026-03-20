use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args();
    let program_name = args.next().unwrap_or_else(|| "chacc".to_owned());
    let argv: Vec<String> = args.collect();
    let assembly = compile_from_args(&program_name, &argv)?;

    print!("{assembly}");
    Ok(())
}

fn compile_from_args(program_name: &str, args: &[String]) -> Result<String, String> {
    let [input] = args else {
        return Err(format!("{program_name}: invalid number of arguments"));
    };

    let value: i32 = input
        .parse()
        .map_err(|_| format!("{program_name}: invalid integer: {input}"))?;

    Ok(compile_integer_program(value))
}

fn compile_integer_program(value: i32) -> String {
    format!("  .globl main\nmain:\n  mov ${value}, %rax\n  ret\n")
}

#[cfg(test)]
mod tests {
    use super::{compile_from_args, compile_integer_program};
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn emits_expected_assembly() {
        assert_eq!(
            compile_integer_program(42),
            "  .globl main\nmain:\n  mov $42, %rax\n  ret\n"
        );
    }

    #[test]
    fn rejects_the_wrong_number_of_arguments() {
        let error = compile_from_args("chacc", &[]).unwrap_err();
        assert_eq!(error, "chacc: invalid number of arguments");
    }

    #[test]
    fn generated_binary_exits_with_the_input_value() {
        for value in [0, 42] {
            let dir = unique_test_dir();
            fs::create_dir_all(&dir).unwrap();

            let assembly_path = dir.join("tmp.s");
            let executable_path = dir.join("tmp");
            fs::write(&assembly_path, compile_integer_program(value)).unwrap();

            let status = Command::new("cc")
                .arg("-o")
                .arg(&executable_path)
                .arg(&assembly_path)
                .status()
                .unwrap();
            assert!(status.success(), "cc failed to assemble {assembly_path:?}");

            let status = Command::new(&executable_path).status().unwrap();
            assert_eq!(status.code(), Some(value));

            fs::remove_dir_all(dir).unwrap();
        }
    }

    fn unique_test_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        env::temp_dir().join(format!("chacc-{nanos}"))
    }
}
