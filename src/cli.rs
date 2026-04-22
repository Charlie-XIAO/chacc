//! The CLI interface of chacc.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

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
    println!("  -save-temps         Keep intermediate files.");
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
    pub save_temps: bool,
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
    save_temps: bool,
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
            if partial.compile_only {
                Path::new(input.file_stem().unwrap_or(OsStr::new("in"))).with_extension("s")
            } else {
                PathBuf::from("a.out")
            }
        });

        Ok(Cli {
            input,
            output,
            compile_only: partial.compile_only,
            tool_search_paths: partial.tool_search_paths,
            save_temps: partial.save_temps,
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

    /// Get a temporary output file path with the given extension.
    ///
    /// If `base` is provided, the temporary file will be created in that
    /// directory; otherwise, it will be created in the same directory as the
    /// final output file.
    pub fn temp_output_path(&self, ext: &str, base: Option<&Path>) -> PathBuf {
        let mut file_name = self
            .output
            .file_stem()
            .unwrap_or(OsStr::new("out"))
            .to_owned();
        file_name.push("-");
        file_name.push(self.input.file_stem().unwrap_or(OsStr::new("in")));

        let path = if let Some(base) = base {
            base.join(file_name)
        } else {
            self.output.with_file_name(file_name)
        };
        path.with_extension(ext)
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
