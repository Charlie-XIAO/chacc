//! The chacc compiler proper (cc1).

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::cli::Cli;
use crate::codegen::Codegen;
use crate::error::Result;
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
        let tokens = Tokenizer::new(source).tokenize(true)?;

        let includes = self.include_paths()?;

        if self.cli.preprocess_only {
            let mut sink = PreprocessedWriter::new(self.out);
            Preprocessor::new(&mut source_map, &includes, tokens, &mut sink).preprocess(true)?;
            return Ok(());
        }

        let mut sink = PreprocessedTokens::default();
        Preprocessor::new(&mut source_map, &includes, tokens, &mut sink).preprocess(true)?;

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
    /// - The paths specified by the `C_INCLUDE_PATH` environment variable;
    /// - The system include paths, probed from the host C compiler.
    fn include_paths(&self) -> Result<Vec<PathBuf>> {
        let cpath = std::env::var_os("CPATH");
        let c_include_path = std::env::var_os("C_INCLUDE_PATH");

        let hostcc = Hostcc::resolve()?;
        let system_includes = hostcc.find_system_includes()?;

        Ok(self
            .cli
            .includes
            .iter()
            .cloned()
            .chain(cpath.iter().flat_map(std::env::split_paths))
            .chain(c_include_path.iter().flat_map(std::env::split_paths))
            .chain(system_includes.into_iter())
            .collect())
    }
}
