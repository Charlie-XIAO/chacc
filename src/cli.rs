//! The CLI interface of chacc.

use std::ffi::OsStr;
use std::path::PathBuf;

use lexopt::Arg;

use crate::error::Result;

fn help() {
    println!("Usage: chacc [options] <input>...");
    println!();
    println!("Arguments:");
    println!("  <input>             Input file, or \"-\" for stdin.");
    println!();
    println!("Options:");
    println!("  -o <file>           Output file, or \"-\" for stdout only with -S.");
    println!("  -S                  Compile only; do not assemble or link.");
    println!("  -c                  Compile and assemble, but do not link.");
    println!("  -B <dir>            Add a directory to the search path for tools.");
    println!("  -save-temps         Keep intermediate files.");
    println!("  -###                Print subprocess commands.");
    println!("  --pass-exit-codes   Exit with the highest error code from a phase.");
    println!("  -h, --help          Show this help message.");
    println!();
    println!("Environment variables:");
    println!("  CHACC_HOST_CC       Path to the host C compiler binary used to resolve linker");
    println!("                      startup files and libraries. If unset, chacc tries gcc,");
    println!("                      cc, and clang in order.");
}

#[derive(Debug, Clone, Copy)]
pub enum CliInputKind {
    C,
    Assembler,
    Object,
}

#[derive(Debug)]
pub struct CliInput {
    pub path: PathBuf,
    pub kind: CliInputKind,
}

#[derive(Debug)]
pub struct Cli {
    pub inputs: Vec<CliInput>,
    pub output: Option<PathBuf>,
    pub stop_after_compile: bool,
    pub stop_after_assemble: bool,
    pub tool_search_paths: Vec<PathBuf>,
    pub save_temps: bool,
    pub print_subprocess_commands: bool,
    pub pass_exit_codes: bool,
    pub cc1: bool,
}

#[derive(Debug, Default)]
struct CliPartial {
    inputs: Vec<CliInput>,
    output: Option<PathBuf>,
    stop_after_compile: bool,
    stop_after_assemble: bool,
    tool_search_paths: Vec<PathBuf>,
    save_temps: bool,
    print_subprocess_commands: bool,
    pass_exit_codes: bool,
    cc1: bool,
}

impl TryFrom<CliPartial> for Cli {
    type Error = lexopt::Error;

    fn try_from(cli: CliPartial) -> Result<Self, Self::Error> {
        let CliPartial {
            inputs,
            output,
            stop_after_compile,
            stop_after_assemble,
            tool_search_paths,
            save_temps,
            print_subprocess_commands,
            pass_exit_codes,
            cc1,
        } = cli;

        if cli.cc1 {
            if inputs.len() != 1 {
                return Err("cc1 mode expected exactly one input file".into());
            }
            if output.is_none() {
                return Err("cc1 mode expects an output file".into());
            }
        }

        if !cli.cc1
            && !cli.stop_after_compile
            && let Some(ref output) = output
            && output.as_os_str() == "-"
        {
            return Err("-S required when output is to stdout".into());
        }

        if inputs.is_empty() {
            return Err("no input file".into());
        }

        if inputs.len() > 1 && output.is_some() && (stop_after_compile || stop_after_assemble) {
            return Err("cannot specify '-o' with '-c' or '-S' with multiple files".into());
        }

        Ok(Cli {
            inputs,
            output,
            stop_after_compile,
            stop_after_assemble,
            tool_search_paths,
            save_temps,
            print_subprocess_commands,
            pass_exit_codes,
            cc1,
        })
    }
}

impl Cli {
    /// Parse the CLI from command line.
    pub fn parse() -> Result<Self> {
        Ok(Self::parse_inner()?)
    }

    fn parse_inner() -> Result<Self, lexopt::Error> {
        let mut cli = CliPartial::default();
        let mut parser = lexopt::Parser::from_env();

        loop {
            if let Some(mut raw) = parser.try_raw_args() {
                match raw.peek().and_then(|arg| arg.to_str()) {
                    Some("-save-temps") => {
                        raw.next();
                        cli.save_temps = true;
                        continue;
                    },
                    Some("-###") => {
                        raw.next();
                        cli.print_subprocess_commands = true;
                        continue;
                    },
                    Some("-pass-exit-codes") => {
                        raw.next();
                        cli.pass_exit_codes = true;
                        continue;
                    },
                    Some("-cc1") => {
                        raw.next();
                        cli.cc1 = true;
                        continue;
                    },
                    _ => {},
                }
            }

            let Some(arg) = parser.next()? else {
                break;
            };

            match arg {
                Arg::Short('o') => {
                    cli.output = Some(PathBuf::from(parser.value()?));
                },
                Arg::Short('S') => {
                    cli.stop_after_compile = true;
                },
                Arg::Short('c') => {
                    cli.stop_after_assemble = true;
                },
                Arg::Short('B') => {
                    cli.tool_search_paths.push(PathBuf::from(parser.value()?));
                },
                Arg::Short('h') | Arg::Long("help") => {
                    help();
                    std::process::exit(0);
                },
                Arg::Value(path) => {
                    let path = PathBuf::from(path);
                    let kind = match path.extension().and_then(OsStr::to_str) {
                        // TODO: "-" requires either -E or -x, when supported
                        _ if path.as_os_str() == "-" => CliInputKind::C,
                        Some("c") => CliInputKind::C,
                        Some("s") => CliInputKind::Assembler,
                        _ => CliInputKind::Object,
                    };
                    cli.inputs.push(CliInput { path, kind });
                },
                _ => return Err(arg.unexpected()),
            }
        }

        cli.try_into()
    }

    /// Resolve the path of a tool.
    ///
    /// The tool is searched in the search paths in order, and if no match is
    /// found, the tool name itself is returned as a path.
    pub fn resolve_tool(&self, tool: &str) -> PathBuf {
        for prefix in self.tool_search_paths.iter() {
            let path = prefix.join(tool);
            if path.exists() {
                return path;
            }
        }
        PathBuf::from(tool)
    }
}
