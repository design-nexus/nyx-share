use nyx_share_dbus::{ENV_INCOMING, ENV_REQUEST, ENV_TOKEN};
use nyx_share_providers::process;
use std::path::PathBuf;
use std::process::Stdio;

const SHEET: &str = "nyx-share-sheet";

/// The sheet installed beside this service, else the one on `PATH`.
fn sheet_binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("NYX_SHARE_SHEET").map(PathBuf::from) {
        return Some(p);
    }
    let beside = std::env::current_exe().ok()?.parent()?.join(SHEET);
    if beside.is_file() { Some(beside) } else { process::find(SHEET) }
}

/// Open the Share Sheet for a request. The id and token go in its environment.
pub fn sheet(request: &str, token: &str) -> Result<(), String> {
    open(ENV_REQUEST, request, token)
}

/// Open the Share Sheet to ask about an incoming offer.
pub fn incoming(session: &str, token: &str) -> Result<(), String> {
    open(ENV_INCOMING, session, token)
}

fn open(var: &str, id: &str, token: &str) -> Result<(), String> {
    let binary = sheet_binary().ok_or_else(|| "The Share Sheet isn't installed".to_string())?;
    let mut child = process::detached(&binary)
        .env(var, id)
        .env(ENV_TOKEN, token)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| format!("Couldn't open the Share Sheet: {e}"))?;
    std::thread::spawn(move || child.wait());
    Ok(())
}
