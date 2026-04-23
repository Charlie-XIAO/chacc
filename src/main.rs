//! The Cha C compiler (chacc).

mod ast;
mod cli;
mod codegen;
mod constexpr;
mod error;
mod parse;
mod source;
mod tokenize;
mod types;
mod utils;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::rc::Rc;

use tempfile::TempDir;

use crate::cli::{Cli, CliInputKind};
use crate::codegen::Codegen;
use crate::error::{Error, Result};
use crate::parse::Parser;
use crate::source::Source;
use crate::tokenize::Tokenizer;

/// A compilation stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    Compile,
    Assemble,
    Link,
}

impl Stage {
    /// Return the next stage of the current stage.
    fn next(self) -> Option<Self> {
        match self {
            Self::Compile => Some(Self::Assemble),
            Self::Assemble => Some(Self::Link),
            Self::Link => None,
        }
    }
}

impl From<CliInputKind> for Stage {
    /// Convert the CLI input kind to the stage it should start from.
    fn from(kind: CliInputKind) -> Self {
        match kind {
            CliInputKind::C => Stage::Compile,
            CliInputKind::Assembler => Stage::Assemble,
            CliInputKind::Object => Stage::Link,
        }
    }
}

/// A step in the compilation process.
#[derive(Debug)]
enum Step {
    Compile { input: Rc<Path>, output: Rc<Path> },
    Assemble { input: Rc<Path>, output: Rc<Path> },
    Link { input: Rc<Path>, output: Rc<Path> },
}

/// A compilation plan.
#[derive(Debug, Default)]
struct Plan {
    steps: Vec<Step>,
    tempdir: Option<TempDir>,
}

impl Plan {
    /// Return the path of the temporary directory.
    ///
    /// This will create the temporary directory the first time it is called,
    /// and all subsequent calls will return the path to that same directory.
    fn tempdir(&mut self) -> Result<&Path> {
        if self.tempdir.is_none() {
            self.tempdir = Some(TempDir::with_prefix("chacc")?);
        }
        Ok(self.tempdir.as_ref().unwrap().path())
    }
}

/// The chacc compiler driver.
#[derive(Debug)]
struct Driver {
    cli: Cli,
    highest_exit_code: Option<u8>,
}

impl Driver {
    /// Construct a driver from the CLI options.
    fn new(cli: Cli) -> Self {
        Self {
            cli,
            highest_exit_code: None,
        }
    }

    /// Produce the compilation plan.
    fn plan(&self) -> Result<Plan> {
        let mut plan = Plan::default();

        let final_stage = if self.cli.stop_after_compile {
            Stage::Compile
        } else if self.cli.stop_after_assemble {
            Stage::Assemble
        } else {
            Stage::Link
        };

        let mut input = self.cli.input.path.clone();
        let mut current_stage = Some(Stage::from(self.cli.input.kind));

        while let Some(stage) = current_stage
            && stage <= final_stage
        {
            let is_final_stage = stage == final_stage;

            match stage {
                Stage::Compile => {
                    let next = self.stage_output(&mut plan, is_final_stage, "s")?;
                    plan.steps.push(Step::Compile {
                        input,
                        output: next.clone(),
                    });
                    input = next;
                },
                Stage::Assemble => {
                    let next = self.stage_output(&mut plan, is_final_stage, "o")?;
                    plan.steps.push(Step::Assemble {
                        input,
                        output: next.clone(),
                    });
                    input = next;
                },
                Stage::Link => {
                    plan.steps.push(Step::Link {
                        input,
                        output: self.cli.output.clone(),
                    });
                    break;
                },
            }

            current_stage = current_stage.and_then(Stage::next);
        }

        Ok(plan)
    }

    /// Get the output path for a compilation stage with the given extension.
    ///
    /// If this is the final stage, return the final output path. Otherwise,
    /// return a temporary path for the intermediate output.
    ///
    /// If `save_temps` is enabled, the temporary path will be in the same
    /// directory as the final output path. Otherwise, it will be in the
    /// temporary directory determined by the compilation plan.
    fn stage_output(&self, plan: &mut Plan, is_final_stage: bool, ext: &str) -> Result<Rc<Path>> {
        if is_final_stage {
            return Ok(self.cli.output.clone());
        }

        // ${output}-${input}
        let mut file_name = self
            .cli
            .output
            .file_stem()
            .unwrap_or(OsStr::new("out"))
            .to_owned();
        file_name.push("-");
        file_name.push(self.cli.input.path.file_stem().unwrap_or(OsStr::new("in")));

        let path = if self.cli.save_temps {
            self.cli.output.with_file_name(file_name)
        } else {
            plan.tempdir()?.join(file_name)
        };

        Ok(path.with_extension(ext).into())
    }

    /// Run the driver to execute the compilation process.
    fn run(&mut self) -> Result<()> {
        if self.cli.cc1 {
            // Core compilation logic in cc1 mode
            let source = Source::new(&self.cli.input.path)?;
            let tokens = Tokenizer::new(&source).tokenize()?;
            let program = Parser::new(&source, tokens).parse_program()?;
            let codegen = Codegen::new(&source, &self.cli.output)?;
            codegen.generate(program)?;
            return Ok(());
        }

        let plan = self.plan()?;

        for step in plan.steps {
            match step {
                Step::Compile { input, output } => self.run_subprocess("compile", {
                    let mut command = Command::new(std::env::current_exe()?);
                    command.arg("-cc1");
                    command.arg("-o");
                    command.arg(output.as_ref());
                    command.arg(input.as_ref());
                    command
                })?,
                Step::Assemble { input, output } => self.run_subprocess("assemble", {
                    let mut command = Command::new(self.cli.resolve_tool("as"));
                    command.arg("-c");
                    command.arg(input.as_ref());
                    command.arg("-o");
                    command.arg(output.as_ref());
                    command
                })?,
                Step::Link { input, output } => self.run_subprocess("link", {
                    let hostcc = Hostcc::resolve()?;
                    let mut command = Command::new(self.cli.resolve_tool("ld"));
                    command.arg("-o");
                    command.arg(output.as_ref());
                    command.arg("-m");
                    command.arg("elf_x86_64");
                    command.arg("-dynamic-linker");
                    command.arg(hostcc.find("ld-linux-x86-64.so.2")?);
                    command.arg(hostcc.find("crt1.o")?);
                    command.arg(hostcc.find("crti.o")?);
                    command.arg(hostcc.find("crtbegin.o")?);
                    command.arg(input.as_ref());
                    command.arg(hostcc.find("libc.so")?);
                    command.arg(hostcc.find("libgcc.a")?);
                    command.arg("--as-needed");
                    command.arg(hostcc.find("libgcc_s.so.1")?);
                    command.arg("--no-as-needed");
                    command.arg(hostcc.find("crtend.o")?);
                    command.arg(hostcc.find("crtn.o")?);
                    command
                })?,
            }
        }

        Ok(())
    }

    /// Get the highest exit code recorded, only if requested.
    fn code(&self) -> Option<ExitCode> {
        if !self.cli.pass_exit_codes {
            return None;
        }
        self.highest_exit_code.map(ExitCode::from)
    }

    /// Run a subprocess command.
    fn run_subprocess(&mut self, name: &str, mut command: Command) -> Result<()> {
        if self.cli.print_subprocess_commands {
            eprintln!("{name}: {command:?}");
        }

        let status = command.status()?;
        if status.success() {
            return Ok(());
        }

        if self.cli.pass_exit_codes {
            self.highest_exit_code = self
                .highest_exit_code
                .max(status.code().map(|code| code as u8));
        }
        Err(Error::Terminate)
    }
}

/// The host C compiler.
struct Hostcc(PathBuf);

impl Hostcc {
    /// Resolve the host C compiler to use.
    ///
    /// This will first check the `CHACC_HOST_CC` environment variable, then try
    /// to find `gcc`, `cc`, and `clang` executables in order.
    pub fn resolve() -> Result<Self> {
        let path = if let Some(hostcc) = std::env::var_os("CHACC_HOST_CC") {
            which::which(&hostcc).map_err(|e| {
                Error::HostccNotFound(format!("CHACC_HOST_CC='{}': {e}", hostcc.display()))
            })?
        } else if let Ok(gcc) = which::which("gcc") {
            gcc
        } else if let Ok(cc) = which::which("cc") {
            cc
        } else if let Ok(clang) = which::which("clang") {
            clang
        } else {
            let msg = "either make gcc, cc, or clang discoverable in PATH, or set CHACC_HOST_CC \
                       to a valid C compiler";
            return Err(Error::HostccNotFound(msg.to_string()));
        };
        Ok(Self(path))
    }

    /// Find the library path of a toolchain file.
    fn find(&self, name: &'static str) -> Result<PathBuf> {
        let output = Command::new(&self.0)
            .arg(format!("-print-file-name={name}"))
            .output()?;
        if !output.status.success() {
            return Err(Error::HostccResolutionFailed(name));
        }

        let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if path.is_empty() || path == name {
            return Err(Error::HostccResolutionFailed(name));
        }
        Ok(path.into())
    }
}

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
            } else if driver.cli.cc1 {
                eprintln!("cc1: {e}");
            } else {
                eprintln!("chacc: {e}");
            }
            driver.code().unwrap_or(ExitCode::FAILURE)
        },
    }
}
