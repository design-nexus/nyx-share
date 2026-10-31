use crate::{Data, Outcome, Payload, Provider, SendError, Task, write_error};
use nyx_share_core::{ContentKind, Destination, Section, names, units};
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Save to folder: copies into a folder the user picks. Never moves, never overwrites.
pub struct Folder;

impl Provider for Folder {
    fn id(&self) -> &'static str {
        "folder"
    }

    fn name(&self) -> &'static str {
        "Save to folder"
    }

    fn kinds(&self) -> Vec<ContentKind> {
        vec![ContentKind::Text, ContentKind::Image, ContentKind::File]
    }

    fn destinations(&self) -> Vec<Destination> {
        vec![Destination {
            id: "save".into(),
            provider: self.id().into(),
            name: "Save to folder".into(),
            icon: "folder-download-symbolic".into(),
            tint: "graphite".into(),
            section: Section::Apps,
            kinds: self.kinds(),
            max_items: None,
            needs_folder: true,
            available: true,
            status: None,
            pinned: false,
            hidden: false,
        }]
    }

    fn send(&self, _destination: &str, task: &Task<'_>) -> Result<Outcome, SendError> {
        let folder = task.options.folder.as_deref().ok_or_else(|| SendError::failed("Choose a folder to save to"))?;
        if !folder.is_absolute() || !folder.is_dir() {
            return Err(SendError::failed(format!("{} isn't a folder", folder.display())));
        }
        let place = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| folder.display().to_string());
        let needed: u64 = task.items.iter().map(Payload::size).sum();
        if let Some(free) = free_space(folder)
            && free < needed
        {
            return Err(SendError::failed(format!(
                "There isn't enough free space in {place}: {} needed, {} free",
                units::size(needed),
                units::size(free)
            )));
        }
        let mut outputs = Vec::new();
        for item in task.items {
            let name = match item.info.kind {
                ContentKind::Text | ContentKind::Url => names::for_text(Some(&item.info.name)),
                _ => names::sanitize(&item.info.name),
            };
            match save(folder, &name, item, task) {
                Ok(path) => outputs.push(path.display().to_string()),
                Err(SendError::Cancelled { .. }) => return Err(SendError::Cancelled { outputs }),
                Err(SendError::Failed { message, .. }) => {
                    let message = match outputs.len() {
                        0 => message,
                        n => format!("{message}. {n} of {} items were saved.", task.items.len()),
                    };
                    return Err(SendError::Failed { message, outputs });
                }
            }
        }
        let message = match outputs.as_slice() {
            [one] => format!("Saved {} to {place}", Path::new(one).file_name().unwrap_or_default().to_string_lossy()),
            many => format!("Saved {} items to {place}", many.len()),
        };
        Ok(Outcome { message, outputs })
    }
}

fn free_space(dir: &Path) -> Option<u64> {
    let c = CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid C string and `st` a valid out-pointer.
    (unsafe { libc::statvfs(c.as_ptr(), &mut st) } == 0).then(|| st.f_bavail as u64 * st.f_frsize as u64)
}

fn tag() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().subsec_nanos();
    format!("{:x}{:x}{:x}", std::process::id(), nanos, N.fetch_add(1, Ordering::Relaxed))
}

/// Write one item to a hidden partial file, then give it the first free name.
fn save(dir: &Path, name: &str, item: &Payload, task: &Task<'_>) -> Result<PathBuf, SendError> {
    let partial = dir.join(names::partial(name, &tag()));
    let mut out =
        OpenOptions::new().write(true).create_new(true).open(&partial).map_err(|e| SendError::failed(write_error(&e)))?;
    let result = write_all(&mut out, item, task).and_then(|()| {
        out.sync_all().map_err(|e| SendError::failed(write_error(&e)))?;
        drop(out);
        place(&partial, dir, name)
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    } else if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    result
}

fn write_all(out: &mut File, item: &Payload, task: &Task<'_>) -> Result<(), SendError> {
    match &item.data {
        Data::Text(text) => {
            out.write_all(text.as_bytes()).map_err(|e| SendError::failed(write_error(&e)))?;
            (task.progress)(text.len() as u64);
            Ok(())
        }
        Data::File(file) => crate::copy_file(file, item.info.size, out, task),
    }
}

/// Move the finished file to `name`, or "name (2)" and so on if that is taken,
/// without ever replacing an existing file.
fn place(partial: &Path, dir: &Path, name: &str) -> Result<PathBuf, SendError> {
    for n in 1..10_000 {
        let candidate = dir.join(if n == 1 { name.to_string() } else { names::alternative(name, n) });
        match rename_noreplace(partial, &candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(SendError::failed(write_error(&e))),
        }
    }
    Err(SendError::failed(format!("Too many files are already named {name}")))
}

fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    let (a, b) = (cstr(from)?, cstr(to)?);
    // SAFETY: both are valid C strings; AT_FDCWD with absolute paths.
    let r = unsafe { libc::renameat2(libc::AT_FDCWD, a.as_ptr(), libc::AT_FDCWD, b.as_ptr(), libc::RENAME_NOREPLACE) };
    if r == 0 {
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    match e.raw_os_error() {
        // The filesystem can't rename without replacing: a hard link fails on an existing name too.
        Some(libc::EINVAL) | Some(libc::ENOSYS) | Some(libc::EOPNOTSUPP) => match std::fs::hard_link(from, to) {
            Ok(()) => std::fs::remove_file(from),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(e),
            // No hard links either (FAT, some network shares): check, then rename.
            Err(_) if to.symlink_metadata().is_ok() => Err(std::io::ErrorKind::AlreadyExists.into()),
            Err(_) => std::fs::rename(from, to),
        },
        _ => Err(e),
    }
}

fn cstr(p: &Path) -> std::io::Result<CString> {
    CString::new(p.as_os_str().as_bytes()).map_err(|_| std::io::ErrorKind::InvalidInput.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SendOptions;
    use nyx_share_core::ItemInfo;
    use std::sync::atomic::AtomicBool;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nyx-share-test-{name}-{}", tag()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn file_item(src: &Path, name: &str) -> Payload {
        let size = std::fs::metadata(src).unwrap().len();
        Payload {
            info: ItemInfo { kind: ContentKind::File, name: name.into(), mime: None, size, preview: None },
            data: Data::File(File::open(src).unwrap()),
        }
    }

    fn run(items: &[Payload], folder: &Path, cancel: bool) -> Result<Outcome, SendError> {
        let options = SendOptions { folder: Some(folder.to_path_buf()) };
        let cancel = AtomicBool::new(cancel);
        let task = Task { items, options: &options, cancel: &cancel, progress: &|_| {} };
        Folder.send("save", &task)
    }

    #[test]
    fn saves_without_overwriting() {
        let dir = scratch("save");
        let src = dir.join("source.bin");
        std::fs::write(&src, b"new bytes").unwrap();
        let out = dir.join("out");
        std::fs::create_dir(&out).unwrap();
        std::fs::write(out.join("report.pdf"), b"keep me").unwrap();

        let done = run(&[file_item(&src, "report.pdf"), file_item(&src, "report.pdf")], &out, false).unwrap();
        assert_eq!(std::fs::read(out.join("report.pdf")).unwrap(), b"keep me");
        assert_eq!(std::fs::read(out.join("report (2).pdf")).unwrap(), b"new bytes");
        assert_eq!(std::fs::read(out.join("report (3).pdf")).unwrap(), b"new bytes");
        assert_eq!(done.outputs.len(), 2);
        assert_eq!(done.message, "Saved 2 items to out");
        assert!(std::fs::read_dir(&out).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with("partial")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn keeps_names_inside_the_folder() {
        let dir = scratch("traversal");
        let src = dir.join("s");
        std::fs::write(&src, b"x").unwrap();
        let out = dir.join("out");
        std::fs::create_dir(&out).unwrap();
        run(&[file_item(&src, "../../escape.txt")], &out, false).unwrap();
        assert!(out.join("escape.txt").exists());
        assert!(!dir.join("escape.txt").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saves_text_and_cleans_up_when_cancelled() {
        let dir = scratch("text");
        let text = Payload {
            info: ItemInfo { kind: ContentKind::Text, name: "Shared text".into(), mime: None, size: 5, preview: None },
            data: Data::Text("hello".into()),
        };
        run(std::slice::from_ref(&text), &dir, false).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("Shared text.txt")).unwrap(), "hello");

        let src = dir.join("big");
        std::fs::write(&src, vec![7u8; 3 << 20]).unwrap();
        let out = dir.join("out");
        std::fs::create_dir(&out).unwrap();
        assert!(matches!(run(&[file_item(&src, "big")], &out, true), Err(SendError::Cancelled { .. })));
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refuses_a_missing_folder() {
        let items = [Payload {
            info: ItemInfo { kind: ContentKind::Text, name: "t".into(), mime: None, size: 1, preview: None },
            data: Data::Text("x".into()),
        }];
        assert!(matches!(run(&items, Path::new("/nonexistent/nyx-share"), false), Err(SendError::Failed { .. })));
    }
}
