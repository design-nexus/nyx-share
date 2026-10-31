//! Destinations built into the service. Each provider lists its destinations and
//! sends a job's items to one of them on a worker thread.

mod clipboard;
mod folder;
pub mod process;

pub use clipboard::Clipboard;
pub use folder::{Folder, free_bytes, save_stream};

use nyx_share_core::{ContentKind, Destination, ItemInfo};
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// One accepted item: what it is, and its data.
pub struct Payload {
    pub info: ItemInfo,
    pub data: Data,
}

pub enum Data {
    Text(String),
    /// Read with positional reads only: the descriptor's offset may be shared.
    File(File),
}

impl Payload {
    pub fn size(&self) -> u64 {
        match &self.data {
            Data::Text(t) => t.len() as u64,
            Data::File(_) => self.info.size,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SendOptions {
    pub folder: Option<PathBuf>,
}

/// A job as a provider sees it.
pub struct Task<'a> {
    pub items: &'a [Payload],
    pub options: &'a SendOptions,
    pub cancel: &'a AtomicBool,
    /// Called with the number of bytes just sent.
    pub progress: &'a (dyn Fn(u64) + Sync),
}

impl Task<'_> {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub message: String,
    pub outputs: Vec<String>,
}

#[derive(Debug)]
pub enum SendError {
    Cancelled { outputs: Vec<String> },
    Failed { message: String, outputs: Vec<String> },
}

impl SendError {
    pub fn failed(message: impl Into<String>) -> Self {
        Self::Failed { message: message.into(), outputs: Vec::new() }
    }
}

pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn kinds(&self) -> Vec<ContentKind>;
    /// The destinations it offers now, with their availability.
    fn destinations(&self) -> Vec<Destination>;
    fn send(&self, destination: &str, task: &Task<'_>) -> Result<Outcome, SendError>;
}

/// The providers in this release.
pub fn builtin() -> Vec<Box<dyn Provider>> {
    vec![Box::new(Clipboard), Box::new(Folder)]
}

/// Stream a file to `out` in 1 MiB positional reads, reporting progress and
/// stopping when the task is cancelled.
pub(crate) fn copy_file(file: &File, size: u64, out: &mut impl std::io::Write, task: &Task<'_>) -> Result<(), SendError> {
    let mut buf = vec![0u8; 1 << 20];
    let mut offset = 0u64;
    loop {
        if task.cancelled() {
            return Err(SendError::Cancelled { outputs: Vec::new() });
        }
        let n = match file.read_at(&mut buf, offset) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(SendError::failed(format!("Couldn't read the file: {e}"))),
        };
        out.write_all(&buf[..n]).map_err(|e| SendError::failed(write_error(&e)))?;
        offset += n as u64;
        (task.progress)(n as u64);
    }
    if offset != size {
        return Err(SendError::failed("The file changed while it was being shared"));
    }
    Ok(())
}

pub(crate) fn write_error(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(libc::ENOSPC) | Some(libc::EDQUOT) => "There isn't enough free space".into(),
        Some(libc::EACCES) | Some(libc::EPERM) | Some(libc::EROFS) => "That folder can't be written to".into(),
        _ => format!("Couldn't write the file: {e}"),
    }
}
