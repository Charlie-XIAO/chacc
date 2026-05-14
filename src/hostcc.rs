//! Host C compiler resolution and querying.
//!
//! Note that chacc relies on the host C compiler only for toolchain resolution
//! and some other metadata. It does not use it for actual compilation. chacc
//! chose this design to have better support for different Linux distros without
//! needing to hardcode a lot of platform-specific details.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::error::{Error, Result};

/// The host C compiler.
pub struct Hostcc(PathBuf);

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
    pub fn find(&self, name: &'static str) -> Result<PathBuf> {
        let output = Command::new(&self.0)
            .arg(format!("-print-file-name={name}"))
            .output()?;
        if !output.status.success() {
            return Err(Error::HostccResolutionFailed(name.to_string()));
        }

        let path = OsStr::from_bytes(output.stdout.trim_ascii());
        if path.is_empty() || path == name {
            return Err(Error::HostccResolutionFailed(name.to_string()));
        }

        std::path::absolute(path).map_err(|e| {
            Error::HostccResolutionFailed(format!(
                "{name}: failed to absolutize '{}': {e}",
                path.display()
            ))
        })
    }

    /// Find the system include paths of the host C compiler.
    pub fn find_system_includes(&self) -> Result<Vec<PathBuf>> {
        let output = Command::new(&self.0)
            .arg("-E")
            .arg("-xc")
            .arg("-v")
            .arg("-")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .env_remove("CPATH")
            .env_remove("C_INCLUDE_PATH")
            .env("LC_ALL", "C")
            .output()?;
        if !output.status.success() {
            return Err(Error::HostccResolutionFailed(
                "system include paths".to_string(),
            ));
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut headers = Vec::new();
        let mut in_block = false;

        for line in stderr.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            if line.starts_with("#include <...> search starts here:") {
                in_block = true;
                continue;
            }
            if line.starts_with("End of search list.") {
                break;
            }

            if in_block {
                let path = std::path::absolute(line).map_err(|e| {
                    Error::HostccResolutionFailed(format!(
                        "system include paths: failed to absolutize '{line}': {e}",
                    ))
                })?;
                headers.push(path);
            }
        }

        if !in_block {
            return Err(Error::HostccResolutionFailed(
                "system include paths: unexpected format".to_string(),
            ));
        }

        Ok(headers)
    }
}
