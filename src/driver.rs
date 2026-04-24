//! The chacc compiler driver.
//!
//! Similar to gcc, apart from being a C compiler, chacc is also a higher-level
//! driver that manages the entire compilation process, which involves cc1
//! (chacc's actual compiler proper), [as] (the GNU assembler), and [ld] (the
//! GNU linker).
//!
//! [as]: https://man7.org/linux/man-pages/man1/as.1.html
//! [ld]: https://man7.org/linux/man-pages/man1/ld.1.html

use std::ffi::OsStr;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use rustc_hash::{FxHashMap, FxHasher};
use tempfile::TempDir;

use crate::cli::{Cli, CliInput, CliInputKind};
use crate::codegen::Codegen;
use crate::error::{Error, Result};
use crate::parse::Parser;
use crate::preprocess::Preprocessor;
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

    /// Return the file extension for the output of the current stage.
    fn ext(self) -> &'static str {
        match self {
            Self::Compile => "s",
            Self::Assemble => "o",
            Self::Link => "",
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

/// A single-file compilation job.
#[derive(Debug, Default)]
struct Job {
    /// Input and output for compilation, if needed.
    compile: Option<(PathBuf, PathBuf)>,
    /// Input and output for assembling, if needed.
    assemble: Option<(PathBuf, PathBuf)>,
    tempdir: Option<TempDir>,
}

impl Job {
    /// Return whether the job is no-op.
    fn is_noop(&self) -> bool {
        self.compile.is_none() && self.assemble.is_none()
    }

    /// Return the path of the temporary directory.
    ///
    /// This will create the temporary directory the first time it is called,
    /// and all subsequent calls will return the path to that same directory.
    fn tempdir(&mut self) -> Result<&Path> {
        if self.tempdir.is_none() {
            self.tempdir = Some(TempDir::with_prefix("chacc-")?);
        }
        Ok(self.tempdir.as_ref().unwrap().path())
    }
}

/// The overall compilation plan.
#[derive(Debug, Default)]
struct Plan {
    jobs: Vec<Job>,
    /// Inputs and output for linking, if needed.
    link: Option<(Vec<PathBuf>, PathBuf)>,
}

/// The chacc compiler driver.
#[derive(Debug)]
pub struct Driver {
    cli: Cli,
    highest_exit_code: Option<u8>,
}

impl Driver {
    /// Construct a driver from the CLI options.
    pub fn new(cli: Cli) -> Self {
        Self {
            cli,
            highest_exit_code: None,
        }
    }

    /// Return whether the driver runs in cc1 mode.
    pub fn cc1(&self) -> bool {
        self.cli.cc1
    }

    /// Return the highest exit code recorded, only if requested.
    pub fn code(&self) -> Option<ExitCode> {
        if !self.cli.pass_exit_codes {
            return None;
        }
        self.highest_exit_code.map(ExitCode::from)
    }

    /// Run the driver to execute the compilation process.
    pub fn run(&mut self) -> Result<()> {
        if self.cli.cc1 {
            // Core compilation logic in cc1 mode; the CLI guarantees that there
            // is exactly one input and one output
            let source = Source::new(&self.cli.inputs[0].path)?;
            let tokens = Tokenizer::new(&source).tokenize()?;
            let tokens = Preprocessor::new(tokens).preprocess()?;
            let program = Parser::new(&source, tokens).parse_program()?;
            let codegen = Codegen::new(&source, self.cli.output.as_ref().unwrap())?;
            codegen.generate(program)?;
            return Ok(());
        }

        let plan = self.plan()?;

        // Note: We must keep jobs alive so that the temporary directories do
        // not get deleted until the end of the compilation process
        for job in &plan.jobs {
            if let Some((input, output)) = &job.compile {
                self.run_subprocess("compile", {
                    let mut command = Command::new(std::env::current_exe()?);
                    command.arg("-cc1");
                    command.arg("-o");
                    command.arg(output);
                    command.arg(input);
                    command
                })?;
            }

            if let Some((input, output)) = &job.assemble {
                self.run_subprocess("assemble", {
                    let mut command = Command::new(self.cli.resolve_tool("as"));
                    command.arg("-c");
                    command.arg(input);
                    command.arg("-o");
                    command.arg(output);
                    command
                })?;
            }
        }

        if let Some((inputs, output)) = &plan.link {
            let hostcc = Hostcc::resolve()?;
            self.run_subprocess("link", {
                let mut command = Command::new(self.cli.resolve_tool("ld"));
                command.arg("-o");
                command.arg(output);
                command.arg("-m");
                command.arg("elf_x86_64");
                command.arg("-dynamic-linker");
                command.arg(hostcc.find("ld-linux-x86-64.so.2")?);
                command.arg(hostcc.find("crt1.o")?);
                command.arg(hostcc.find("crti.o")?);
                command.arg(hostcc.find("crtbegin.o")?);
                for input in inputs {
                    command.arg(input);
                }
                command.arg(hostcc.find("libc.so")?);
                command.arg(hostcc.find("libgcc.a")?);
                command.arg("--as-needed");
                command.arg(hostcc.find("libgcc_s.so.1")?);
                command.arg("--no-as-needed");
                command.arg(hostcc.find("crtend.o")?);
                command.arg(hostcc.find("crtn.o")?);
                command
            })?;
        }

        Ok(())
    }

    /// Produce the compilation plan.
    fn plan(&self) -> Result<Plan> {
        let mut plan = Plan::default();

        fn input_stem(input: &CliInput) -> &OsStr {
            input.path.file_stem().unwrap_or(OsStr::new("chacc-in"))
        }

        // Count the number of input files with the same stem to disambiguate
        // them when generating temporary output paths
        let mut stem_counts = FxHashMap::default();
        for input in &self.cli.inputs {
            let stem = input_stem(input).to_owned();
            *stem_counts.entry(stem).or_insert(0) += 1;
        }

        let final_stage = if self.cli.stop_after_compile {
            Stage::Compile
        } else if self.cli.stop_after_assemble {
            Stage::Assemble
        } else {
            Stage::Link
        };

        let final_link_output = self
            .cli
            .output
            .clone()
            .unwrap_or_else(|| PathBuf::from("a.out"));

        let mut link_inputs = Vec::new();

        for input in &self.cli.inputs {
            let start_stage = Stage::from(input.kind);
            if start_stage > final_stage {
                continue;
            }

            let mut input_tag = input_stem(input).to_owned();
            if stem_counts.get(&input_tag).copied().unwrap_or(0) > 1 {
                // If there is a collision in stem, hash the full input path so
                // that the tag remains unique but is still stable across runs
                let mut hasher = FxHasher::default();
                input.path.hash(&mut hasher);
                let hash = hasher.finish();
                input_tag.push("-");
                input_tag.push(format!("{hash:08x}"));
            }

            let mut job = Job::default();
            let mut current = input.path.clone();
            let mut current_stage = Some(start_stage);

            while let Some(stage) = current_stage
                && stage <= final_stage
                && stage < Stage::Link
            {
                let next = if stage == final_stage {
                    // Final non-linking stage, either output path if provided,
                    // or use input path with the appropriate extension
                    self.cli
                        .output
                        .clone()
                        .unwrap_or_else(|| input.path.with_extension(stage.ext()))
                } else if !self.cli.save_temps {
                    // Intermediate stage without keeping temps, just create
                    // "out.*" because we have per-job temporary directory and
                    // there would never be collisions
                    job.tempdir()?.join("out").with_extension(stage.ext())
                } else if final_stage == Stage::Link {
                    // Intermediate stage, and final stage is linking, then we
                    // use "$output-$input" as the stage output name, and keep
                    // it in the same directory as the final linking output
                    let mut stem = final_link_output
                        .file_stem()
                        .unwrap_or(OsStr::new("a.out"))
                        .to_owned();
                    stem.push("-");
                    stem.push(&input_tag);
                    final_link_output
                        .with_file_name(stem)
                        .with_extension(stage.ext())
                } else {
                    // Intermediate stage, and final stage is not linking, then
                    // infer from either output or input path with the
                    // appropriate extension
                    self.cli
                        .output
                        .clone()
                        .unwrap_or_else(|| input.path.clone())
                        .with_extension(stage.ext())
                };

                let job_stage = match stage {
                    Stage::Compile => &mut job.compile,
                    Stage::Assemble => &mut job.assemble,
                    Stage::Link => unreachable!(),
                };
                *job_stage = Some((current, next.clone()));
                current = next;
                current_stage = stage.next();
            }

            if !job.is_noop() {
                plan.jobs.push(job);
            }

            // Push "current" to link inputs; if no stages were run, this is
            // just the original input that should directly go to the linker;
            // otherwise this is the output of the last stage that was performed
            // and should be linked
            link_inputs.push(current);
        }

        if final_stage == Stage::Link {
            plan.link = Some((link_inputs, final_link_output));
        }

        Ok(plan)
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
