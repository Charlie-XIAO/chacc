//! The CLI interface of chacc.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use lexopt::Arg;

use crate::error::Result;

fn help() {
    println!("Usage: chacc [options] <input>");
    println!();
    println!("Arguments:");
    println!("  <input>             Input path, or \"-\" for stdin.");
    println!();
    println!("Options:");
    println!("  -o/--output <path>  Output path, or \"-\" for stdout.");
    println!("  -S                  Compile only; do not assemble or link.");
    println!("  -B <dir>            Add a directory to the search path for tools.");
    println!("  -###                Print subprocess commands.");
    println!("  --pass-exit-codes   Exit with the highest error code from a phase.");
    println!("  -h, --help          Show this help message.");
}

#[derive(Debug)]
pub struct Cli {
    pub input: PathBuf,
    pub output: PathBuf,
    pub compile_only: bool,
    pub tool_search_paths: Vec<PathBuf>,
    pub print_subprocess_commands: bool,
    pub pass_exit_codes: bool,
    pub cc1: bool,
}

#[derive(Debug, Default)]
struct CliPartial {
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    compile_only: bool,
    tool_search_paths: Vec<PathBuf>,
    print_subprocess_commands: bool,
    pass_exit_codes: bool,
    cc1: bool,
}

impl TryFrom<CliPartial> for Cli {
    type Error = &'static str;

    fn try_from(partial: CliPartial) -> Result<Self, Self::Error> {
        let CliPartial { input, output, .. } = partial;

        let input = input.ok_or("no input file")?;

        let output = output.unwrap_or_else(|| {
            if input.as_os_str() == "-" {
                return PathBuf::from("a.out");
            }

            let with_ext = |ext: &str| {
                let file_name = input
                    .file_name()
                    .and_then(|name| name.to_str())
                    .expect("input path should have a file name");
                let stem = file_name
                    .rsplit_once('.')
                    .map_or(file_name, |(stem, _)| stem);
                PathBuf::from(format!("{stem}{ext}"))
            };

            if partial.compile_only {
                with_ext(".s")
            } else {
                with_ext(".o")
            }
        });

        Ok(Cli {
            input,
            output,
            compile_only: partial.compile_only,
            tool_search_paths: partial.tool_search_paths,
            print_subprocess_commands: partial.print_subprocess_commands,
            pass_exit_codes: partial.pass_exit_codes,
            cc1: partial.cc1,
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
                Arg::Short('o') | Arg::Long("output") => {
                    cli.output = Some(PathBuf::from(parser.value()?));
                },
                Arg::Short('S') => {
                    cli.compile_only = true;
                },
                Arg::Short('B') => {
                    cli.tool_search_paths.push(PathBuf::from(parser.value()?));
                },
                Arg::Short('h') | Arg::Long("help") => {
                    help();
                    std::process::exit(0);
                },
                Arg::Value(path) => {
                    if cli.input.is_some() {
                        return Err("multiple input files are not supported yet".into());
                    }
                    cli.input = Some(PathBuf::from(path));
                },
                _ => return Err(arg.unexpected()),
            }
        }

        Ok(cli.try_into()?)
    }

    /// Get a temporary output file name with the given extension.
    pub fn temp_output(&self, ext: &str) -> OsString {
        let mut path = self
            .output
            .file_stem()
            .unwrap_or(OsStr::new("tmp"))
            .to_owned();
        path.push("--");
        path.push(ext);
        path
    }

    /// Resolve the path of a tool.
    ///
    /// The tool is searched in the search paths in order, and if no match is
    /// found, the tool name itself is returned as a path.
    pub fn resolve_tool(&self, tool: &str) -> PathBuf {
        for prefix in &self.tool_search_paths {
            let path = prefix.join(tool);
            if path.exists() {
                return path;
            }
        }
        PathBuf::from(tool)
    }
}
