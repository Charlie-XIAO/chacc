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

    compile_expression_program(input)
}

fn compile_expression_program(input: &str) -> Result<String, String> {
    let mut parser = Parser::new(input);
    let mut assembly = String::from("  .globl main\nmain:\n");

    let value = parser.read_number();
    assembly.push_str(&format!("  mov ${value}, %rax\n"));

    while let Some(ch) = parser.peek() {
        match ch {
            '+' => {
                parser.advance();
                let value = parser.read_number();
                assembly.push_str(&format!("  add ${value}, %rax\n"));
            }
            '-' => {
                parser.advance();
                let value = parser.read_number();
                assembly.push_str(&format!("  sub ${value}, %rax\n"));
            }
            _ => return Err(format!("unexpected character: '{ch}'")),
        }
    }

    assembly.push_str("  ret\n");
    Ok(assembly)
}

struct Parser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn advance(&mut self) {
        if let Some(ch) = self.peek() {
            self.pos += ch.len_utf8();
        }
    }

    fn read_number(&mut self) -> i64 {
        let start = self.pos;
        let mut cursor = self.pos;
        let bytes = self.input.as_bytes();

        if let Some(sign) = bytes.get(cursor) {
            if matches!(sign, b'+' | b'-') {
                cursor += 1;
            }
        }

        let digits_start = cursor;
        while let Some(byte) = bytes.get(cursor) {
            if byte.is_ascii_digit() {
                cursor += 1;
            } else {
                break;
            }
        }

        if cursor == digits_start {
            return 0;
        }

        self.pos = cursor;
        self.input[start..cursor].parse().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::{compile_expression_program, compile_from_args};

    #[test]
    fn emits_expected_assembly() {
        assert_eq!(
            compile_expression_program("5+20-4").unwrap(),
            "  .globl main\nmain:\n  mov $5, %rax\n  add $20, %rax\n  sub $4, %rax\n  ret\n"
        );
    }

    #[test]
    fn rejects_the_wrong_number_of_arguments() {
        let error = compile_from_args("chacc", &[]).unwrap_err();
        assert_eq!(error, "chacc: invalid number of arguments");
    }

    #[test]
    fn rejects_unexpected_characters() {
        let error = compile_expression_program("5a").unwrap_err();
        assert_eq!(error, "unexpected character: 'a'");
    }
}
