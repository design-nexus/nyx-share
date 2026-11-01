//! NAS passwords in the Secret Service, through `secret-tool`. The password
//! goes over stdin, never on a command line or into a settings file.

use std::io::Write;
use std::process::{Command, Stdio};

/// For checks on a private bus with no keyring: `NYX_SHARE_SECRETS_DIR` keeps
/// each password in a 0600 file named after the folder's id.
fn test_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("NYX_SHARE_SECRETS_DIR").map(std::path::PathBuf::from).filter(|p| p.is_absolute())
}

fn attrs(id: &str) -> [&str; 4] {
    ["application", "nyx-share", "nas", id]
}

pub fn password(id: &str) -> Option<String> {
    if let Some(dir) = test_dir() {
        return std::fs::read_to_string(dir.join(id)).ok();
    }
    let out = Command::new("secret-tool").arg("lookup").args(attrs(id)).stderr(Stdio::null()).output().ok()?;
    let p = String::from_utf8(out.stdout).ok()?;
    (out.status.success() && !p.is_empty()).then_some(p)
}

pub fn store(id: &str, label: &str, password: &str) -> Result<(), String> {
    if let Some(dir) = test_dir() {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).map_err(|e| e.to_string())?;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(dir.join(id))
            .map_err(|e| e.to_string())?;
        return f.write_all(password.as_bytes()).map_err(|e| e.to_string());
    }
    let mut child = Command::new("secret-tool")
        .args(["store", "--label", &format!("Share: {label}")])
        .args(attrs(id))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Couldn't reach the keyring (secret-tool): {e}"))?;
    child.stdin.take().expect("piped").write_all(password.as_bytes()).map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("The keyring refused the password: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

pub fn clear(id: &str) {
    if let Some(dir) = test_dir() {
        let _ = std::fs::remove_file(dir.join(id));
        return;
    }
    let _ = Command::new("secret-tool").arg("clear").args(attrs(id)).stderr(Stdio::null()).status();
}
