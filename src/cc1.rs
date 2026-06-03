//! The chacc compiler proper (cc1).

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::cli::Cli;
use crate::codegen::Codegen;
use crate::error::{Error, Result};
use crate::flock::FileLock;
use crate::hostcc::Hostcc;
use crate::parse::Parser;
use crate::preprocess::{PreprocessedTokens, PreprocessedWriter, Preprocessor};
use crate::source::SourceMap;
use crate::tokenize::Tokenizer;

/// The cc1 runner.
pub struct CC1<'a, W: Write> {
    cli: &'a Cli,
    out: &'a mut W,
}

impl<'a, W: Write> CC1<'a, W> {
    /// Create a new cc1 runner.
    pub fn new(cli: &'a Cli, out: &'a mut W) -> Self {
        Self { cli, out }
    }

    /// Run cc1 on the given input file.
    pub fn run(&mut self, input: &Path) -> Result<()> {
        let mut source_map = SourceMap::default();
        let source = source_map.push(input)?;
        let tokens = Tokenizer::new(source).tokenize(true, true)?;

        let includes = self.include_paths()?;

        if self.cli.preprocess_only {
            let mut sink = PreprocessedWriter::new(self.out);
            Preprocessor::new(
                &mut source_map,
                &includes,
                &self.cli.macro_ops,
                tokens,
                &mut sink,
            )?
            .preprocess(true)?;
            return Ok(());
        }

        let mut sink = PreprocessedTokens::default();
        Preprocessor::new(
            &mut source_map,
            &includes,
            &self.cli.macro_ops,
            tokens,
            &mut sink,
        )?
        .preprocess(true)?;

        let tokens = sink.lower(&source_map)?;
        let program = Parser::new(&source_map, tokens, false).parse_program()?;
        Codegen::new(&source_map, self.out)?.generate(program)?;

        Ok(())
    }

    /// Return the include paths in precedence order.
    ///
    /// This includes:
    ///
    /// - The paths specified by the `-I` flag;
    /// - The paths specified by the `CPATH` environment variable;
    /// - The built-in chacc headers directory;
    /// - The paths specified by the `C_INCLUDE_PATH` environment variable;
    /// - The system include paths, probed from the host C compiler.
    fn include_paths(&self) -> Result<Vec<PathBuf>> {
        let cpath = std::env::var_os("CPATH");
        let c_include_path = std::env::var_os("C_INCLUDE_PATH");

        let hostcc = Hostcc::resolve()?;
        let system_includes = hostcc.find_system_includes()?;
        let builtin_headers_dir = self
            .ensure_builtin_headers()
            .map_err(Error::BuiltinHeaders)?;

        Ok(self
            .cli
            .includes
            .iter()
            .cloned()
            .chain(cpath.iter().flat_map(std::env::split_paths))
            .chain(std::iter::once(builtin_headers_dir))
            .chain(c_include_path.iter().flat_map(std::env::split_paths))
            .chain(system_includes)
            .collect())
    }

    /// Ensure that the built-in headers are available.
    ///
    /// Returns the path to the built-in headers directory.
    fn ensure_builtin_headers(&self) -> Result<PathBuf, std::io::Error> {
        let abs_env_path = |key: &str| -> Option<PathBuf> {
            let val = std::env::var_os(key)?;
            if val.is_empty() {
                None
            } else {
                let path = PathBuf::from(val);
                path.is_absolute().then_some(path)
            }
        };

        let data_dir = abs_env_path("XDG_DATA_HOME")
            .or_else(|| abs_env_path("HOME").map(|p| p.join(".local/share")))
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "cannot find data directory")
            })?;

        let target_parent = data_dir
            .join(env!("CARGO_PKG_NAME"))
            .join(env!("CARGO_PKG_VERSION"));
        std::fs::create_dir_all(&target_parent)?;

        let target = target_parent.join("include");
        let lock_path = target.with_extension("lock");

        let marker_path = target.join(".complete");
        let hash = env!("BUILTIN_INCLUDE_HEADERS_HASH");
        let marker_is_valid =
            || std::fs::read_to_string(&marker_path).is_ok_and(|marker| marker.trim() == hash);

        if marker_is_valid() {
            return Ok(target);
        }

        let _lock = FileLock::lock(&lock_path)?;

        if marker_is_valid() {
            return Ok(target);
        }

        std::fs::create_dir_all(&target)?;

        for (name, content) in [
            ("float.h", include_str!("../include/float.h")),
            ("stdalign.h", include_str!("../include/stdalign.h")),
            ("stdarg.h", include_str!("../include/stdarg.h")),
            ("stdbool.h", include_str!("../include/stdbool.h")),
            ("stddef.h", include_str!("../include/stddef.h")),
            ("stdnoreturn.h", include_str!("../include/stdnoreturn.h")),
        ] {
            let path = target.join(name);
            std::fs::write(&path, content)?;
        }

        std::fs::write(&marker_path, hash)?;
        Ok(target)
    }
}
