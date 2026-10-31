//! Starting programs that must outlive the service.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Find `program` on `PATH`.
pub fn find(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(program)).find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// A command for a program that keeps running after the service exits: the
/// Share Sheet, or `wl-copy` serving the clipboard. Under systemd it gets its
/// own scope, so stopping the service's unit doesn't stop it.
pub fn detached(program: &Path) -> Command {
    let under_systemd = std::env::var_os("INVOCATION_ID").is_some();
    match find("systemd-run").filter(|_| under_systemd) {
        Some(run) => {
            let mut cmd = Command::new(run);
            cmd.args(["--user", "--scope", "--quiet", "--collect", "--"]).arg(program);
            cmd
        }
        None => Command::new(program),
    }
}
