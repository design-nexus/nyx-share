//! Staging: files handed to another program (Flux, Mail) by path, under the
//! names the user sees, without moving or changing the originals.

use crate::{SendError, Task};
use nyx_share_core::names;
use std::collections::HashSet;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileExt};
use std::path::{Path, PathBuf};

/// A private folder of files handed to another program under their own
/// names, removed when dropped.
pub struct Stage {
    dir: PathBuf,
    names: HashSet<String>,
}

impl Stage {
    /// A new folder under `base`, readable only by the user.
    pub fn new(base: &Path) -> std::io::Result<Self> {
        let dir = base.join(uuid::Uuid::new_v4().simple().to_string());
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        Ok(Self { dir, names: HashSet::new() })
    }

    /// The item under its own name: a hard link to the open file where the
    /// filesystem allows one, else a copy.
    pub fn place(&mut self, file: &std::fs::File, name: &str, size: u64, task: &Task<'_>) -> Result<PathBuf, SendError> {
        let mut chosen = names::sanitize(name);
        let mut n = 2;
        while !self.names.insert(chosen.clone()) {
            chosen = names::alternative(&names::sanitize(name), n);
            n += 1;
        }
        let path = self.dir.join(&chosen);
        if link(file, &path).is_ok() {
            return Ok(path);
        }
        let free = crate::free_bytes(&self.dir).unwrap_or(u64::MAX);
        if free < size.saturating_add(64 << 20) {
            return Err(SendError::failed("There isn't room to prepare the files for Flux"));
        }
        let mut out =
            std::fs::File::create_new(&path).map_err(|e| SendError::failed(format!("Couldn't prepare {chosen}: {e}")))?;
        let mut buf = vec![0u8; 1 << 20];
        let mut offset = 0u64;
        loop {
            if task.cancelled() {
                return Err(SendError::Cancelled { outputs: Vec::new() });
            }
            let read = file.read_at(&mut buf, offset).map_err(|e| SendError::failed(format!("Couldn't read {chosen}: {e}")))?;
            if read == 0 {
                break;
            }
            out.write_all(&buf[..read]).map_err(|e| SendError::failed(format!("Couldn't prepare {chosen}: {e}")))?;
            offset += read as u64;
        }
        Ok(path)
    }
}

impl Stage {
    /// A shared text as a file, under `name` (made unique within the stage).
    pub fn write_text(&mut self, name: &str, text: &str) -> Result<PathBuf, SendError> {
        let mut chosen = names::sanitize(name);
        let mut n = 2;
        while !self.names.insert(chosen.clone()) {
            chosen = names::alternative(&names::sanitize(name), n);
            n += 1;
        }
        let path = self.dir.join(&chosen);
        std::fs::write(&path, text).map_err(|e| SendError::failed(format!("Couldn't prepare {chosen}: {e}")))?;
        Ok(path)
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Hard-link an open file to `path` (Linux: through /proc/self/fd).
fn link(file: &std::fs::File, path: &Path) -> std::io::Result<()> {
    let source = std::ffi::CString::new(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
    let target = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: both are valid NUL-terminated paths for the duration of the call.
    let rc = unsafe { libc::linkat(libc::AT_FDCWD, source.as_ptr(), libc::AT_FDCWD, target.as_ptr(), libc::AT_SYMLINK_FOLLOW) };
    if rc == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}
