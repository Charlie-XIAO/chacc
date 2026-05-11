//! The CLI interface of chacc.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use lexopt::Arg;

use crate::error::Result;

/// Print the help message to stdout.
fn help() {
    println!("Usage: chacc [options] <input>...");
    println!();
    println!("Arguments:");
    println!("  <input>             Input file, or \"-\" for stdin.");
    println!();
    println!("Options:");
    println!("  -o <file>           Output file, or \"-\" for stdout only with -E or -S.");
    println!("  -I <dir>            Add a directory to the include search path.");
    println!("  -E                  Only run the preprocessor.");
    println!("  -S                  Only run preprocess and compile stages.");
    println!("  -c                  Only run preprocess, compile, and assemble stages.");
    println!("  -run                Automatically run the compiled program. When specified, ");
    println!("                      arguments after '--' are passed to the compiled program.");
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

/// Input file kind for [`CliInput`].
#[derive(Debug, Clone, Copy)]
pub enum CliInputKind {
    C,
    Assembler,
    Object,
}

/// Input file specified in CLI.
#[derive(Debug)]
pub struct CliInput {
    pub path: PathBuf,
    pub kind: CliInputKind,
}

/// Parsed CLI options and arguments.
#[derive(Debug, Default)]
pub struct Cli {
    pub inputs: Vec<CliInput>,
    pub output: Option<PathBuf>,
    pub includes: Vec<PathBuf>,
    pub preprocess_only: bool,
    pub compile_only: bool,
    pub assemble_only: bool,
    pub auto_run: Option<Vec<OsString>>,
    pub tool_search_paths: Vec<PathBuf>,
    pub save_temps: bool,
    pub print_subprocess_commands: bool,
    pub pass_exit_codes: bool,
    pub cc1: bool,
}

impl Cli {
    /// Parse the CLI from command line.
    pub fn parse() -> Result<Self> {
        let cli = Self::parse_raw()?;
        cli.validate()?;
        Ok(cli)
    }

    /// Parse the CLI from command line without validation.
    fn parse_raw() -> Result<Self, lexopt::Error> {
        let mut cli = Cli::default();
        let mut parser = lexopt::Parser::from_env();

        loop {
            if let Some(mut raw) = parser.try_raw_args() {
                match raw.peek().and_then(|arg| arg.to_str()) {
                    Some("--") if let Some(auto_run) = &mut cli.auto_run => {
                        raw.next();
                        auto_run.extend(raw);
                        break;
                    },
                    Some("-run") => {
                        raw.next();
                        cli.auto_run = Some(Vec::new());
                        continue;
                    },
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
                Arg::Short('I') => {
                    let path = std::path::absolute(parser.value()?)
                        .map_err(|e| format!("failed to resolve '-I': {e}"))?;
                    cli.includes.push(path);
                },
                Arg::Short('E') => {
                    cli.preprocess_only = true;
                },
                Arg::Short('S') => {
                    cli.compile_only = true;
                },
                Arg::Short('c') => {
                    cli.assemble_only = true;
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

        Ok(cli)
    }

    /// Validate the parsed CLI options and arguments.
    fn validate(&self) -> Result<(), lexopt::Error> {
        if self.cc1 {
            if self.inputs.len() != 1 {
                return Err("cc1 mode expected exactly one input file".into());
            }
            if self.output.is_none() {
                return Err("cc1 mode expects an output file".into());
            }
        }

        if !self.cc1
            && !self.preprocess_only
            && !self.compile_only
            && let Some(output) = &self.output
            && output.as_os_str() == "-"
        {
            return Err("'-S' or '-E' required when output is to stdout".into());
        }

        if self.inputs.is_empty() {
            return Err("no input file".into());
        }

        if self.inputs.len() > 1
            && self.output.is_some()
            && (self.preprocess_only || self.compile_only || self.assemble_only)
        {
            return Err("cannot specify '-o' with '-c', '-S', or '-E' with multiple files".into());
        }

        if self.auto_run.is_some()
            && (self.preprocess_only || self.compile_only || self.assemble_only)
        {
            return Err("cannot specify '-run' with '-c', '-S', or '-E'".into());
        }

        Ok(())
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
