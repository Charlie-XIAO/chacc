//! Simple file lock implementation.

use std::os::fd::AsRawFd;
use std::path::Path;

/// A simple [flock]-based file lock.
///
/// [flock]: https://man7.org/linux/man-pages/man2/flock.2.html
pub struct FileLock(std::fs::File);

impl FileLock {
    /// Acquire an exclusive lock on the given path.
    ///
    /// This will create the file if it does not exist.
    pub fn lock(path: &Path) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;

        loop {
            let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if ret == 0 {
                return Ok(Self(file));
            }
            let e = std::io::Error::last_os_error();
            if e.kind() != std::io::ErrorKind::Interrupted {
                return Err(e);
            };
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}
