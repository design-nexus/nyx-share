//! End-to-end checks against a real nyx-share-daemon on a private bus.
//!
//! ```sh
//! cargo build -p nyx-share-daemon
//! dbus-run-session -- cargo test -p nyx-share-client --test service -- --ignored --test-threads=1
//! ```
//!
//! Uses scratch XDG directories, no Wayland (so the clipboard is unavailable and
//! nothing reaches the real one) and a stand-in for the Share Sheet that records
//! the request it was given. Nothing opens on screen.

use nyx_share_client::{Client, Content, ContentKind, Error, JobState, PreferencesUpdate, ShareOptions};
use std::fs::{File, OpenOptions};
use std::os::fd::FromRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

struct Service {
    root: PathBuf,
    _child: Child,
}

fn service() -> &'static Service {
    static S: OnceLock<Service> = OnceLock::new();
    S.get_or_init(|| {
        assert!(std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some(), "run under dbus-run-session");
        let root = std::env::temp_dir().join(format!("nyx-share-service-{}", std::process::id()));
        std::fs::create_dir_all(root.join("config/nyx-share")).unwrap();
        // Nothing on the real network: no discovery, no receiver.
        std::fs::write(root.join("config/nyx-share/nearby.toml"), "visibility = \"off\"\n").unwrap();
        let sheet = root.join("fake-sheet");
        std::fs::write(&sheet, format!("#!/bin/sh\nenv | grep '^NYX_SHARE_' > '{}/sheet-env'\n", root.display())).unwrap();
        std::fs::set_permissions(&sheet, std::fs::Permissions::from_mode(0o755)).unwrap();
        let daemon = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/nyx-share-daemon");
        assert!(daemon.exists(), "build nyx-share-daemon first");
        let child = Command::new(daemon)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("NYX_SHARE_SHEET", &sheet)
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("INVOCATION_ID")
            .spawn()
            .unwrap();
        let start = Instant::now();
        while Client::connect().and_then(|c| c.version()).is_err() {
            assert!(start.elapsed() < Duration::from_secs(10), "the service didn't start");
            std::thread::sleep(Duration::from_millis(50));
        }
        Service { root, _child: child }
    })
}

fn scratch(name: &str) -> PathBuf {
    let dir = service().root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn refused(r: Result<String, Error>, needle: &str) {
    match r {
        Err(Error::Refused(m)) => assert!(m.contains(needle), "{m:?} should mention {needle:?}"),
        other => panic!("expected a refusal mentioning {needle:?}, got {other:?}"),
    }
}

#[test]
#[ignore]
fn reports_version_and_destinations() {
    service();
    let client = Client::connect().unwrap();
    assert_eq!(client.version().unwrap(), 1);
    let caps = client.capabilities().unwrap();
    assert!(caps.providers.iter().any(|p| p.id == "folder" && p.available));
    let clipboard = caps.providers.iter().find(|p| p.id == "clipboard").unwrap();
    assert!(!clipboard.available, "no Wayland in the test session");
    let all = client.destinations(&[], 0).unwrap();
    assert!(all.iter().any(|d| d.id == "save"));
    let for_files = client.destinations(&[ContentKind::File], 3).unwrap();
    assert_eq!(for_files.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["save"]);
    let for_link = client.destinations(&[ContentKind::Url], 1).unwrap();
    assert_eq!(for_link.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["clipboard"]);
}

#[test]
#[ignore]
fn saves_files_and_text_without_overwriting() {
    let dir = scratch("save");
    let src = dir.join("photo.jpg");
    std::fs::write(&src, b"not really a jpeg").unwrap();
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("photo.jpg"), b"existing").unwrap();

    let client = Client::connect().unwrap();
    let mut content = Content::new();
    content.file(&src).unwrap().text("Hello from the test");
    let options = ShareOptions { folder: Some(out.display().to_string()), notify: Some(false), ..Default::default() };
    let id = client.share_to("save", &content, &options).unwrap();
    let job = client.wait(&id, |_| {}).unwrap();
    assert_eq!(job.state, JobState::Completed, "{:?}", job.message);
    assert_eq!(std::fs::read(out.join("photo.jpg")).unwrap(), b"existing");
    assert_eq!(std::fs::read(out.join("photo (2).jpg")).unwrap(), b"not really a jpeg");
    assert_eq!(std::fs::read_to_string(out.join("Shared text.txt")).unwrap(), "Hello from the test");
    assert_eq!(job.outputs.len(), 2);
    assert_eq!(job.bytes_done, job.bytes_total);
    assert!(client.jobs().unwrap().iter().any(|j| j.id == id));
}

#[test]
#[ignore]
fn refuses_unsafe_descriptors() {
    let dir = scratch("fds");
    let path = dir.join("file.txt");
    std::fs::write(&path, b"x").unwrap();
    let client = Client::connect().unwrap();
    let options = ShareOptions { folder: Some(dir.display().to_string()), ..Default::default() };

    let writable = OpenOptions::new().read(true).write(true).open(&path).unwrap();
    let mut content = Content::new();
    content.open_file(writable, ContentKind::File, Some("file.txt".into()), None);
    refused(client.share_to("save", &content, &options), "read-only");

    let mut content = Content::new();
    content.open_file(File::open(&dir).unwrap(), ContentKind::File, None, None);
    refused(client.share_to("save", &content, &options), "folder");

    let c = std::ffi::CString::new(path.display().to_string()).unwrap();
    // SAFETY: a valid path; the descriptor is handed to File.
    let o_path = unsafe { File::from_raw_fd(libc::open(c.as_ptr(), libc::O_PATH | libc::O_CLOEXEC)) };
    let mut content = Content::new();
    content.open_file(o_path, ContentKind::File, None, None);
    refused(client.share_to("save", &content, &options), "O_PATH");

    assert!(matches!(Content::new().file(&dir), Err(Error::Refused(_))));
}

#[test]
#[ignore]
fn refuses_bad_requests() {
    service();
    let client = Client::connect().unwrap();
    let mut link = Content::new();
    link.url("javascript:alert(1)");
    refused(client.share_to("clipboard", &link, &ShareOptions::default()), "link");

    let mut text = Content::new();
    text.text("hi");
    refused(client.share_to("save", &text, &ShareOptions::default()), "folder");
    let relative = ShareOptions { folder: Some("relative/dir".into()), ..Default::default() };
    refused(client.share_to("save", &text, &relative), "absolute");
    assert!(matches!(client.share_to("nowhere", &text, &ShareOptions::default()), Err(Error::NotFound(_))));
    assert!(matches!(client.job("no-such-job"), Err(Error::NotFound(_))));

    let mut file = Content::new();
    let dir = scratch("bad");
    std::fs::write(dir.join("a"), b"a").unwrap();
    file.file(dir.join("a")).unwrap();
    let to_clipboard = client.share_to("clipboard", &file, &ShareOptions::default());
    assert!(matches!(to_clipboard, Err(Error::Failed(_)) | Err(Error::Refused(_))), "{to_clipboard:?}");
}

#[test]
#[ignore]
fn cancels_and_leaves_no_partial_files() {
    let dir = scratch("cancel");
    let big = dir.join("big.bin");
    let f = File::create(&big).unwrap();
    f.set_len(2 << 30).unwrap();
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let client = Client::connect().unwrap();
    let mut content = Content::new();
    content.file(&big).unwrap();
    let options = ShareOptions { folder: Some(out.display().to_string()), notify: Some(false), ..Default::default() };
    let id = client.share_to("save", &content, &options).unwrap();
    client.cancel(&id).unwrap();
    let job = client.wait(&id, |_| {}).unwrap();
    assert_eq!(job.state, JobState::Cancelled, "a 2 GB copy should still be running");
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0, "no partial or finished file is left");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore]
fn share_sheet_requests_need_their_token() {
    let dir = scratch("sheet");
    std::fs::write(dir.join("doc.pdf"), b"%PDF").unwrap();
    let env_file = service().root.join("sheet-env");
    let _ = std::fs::remove_file(&env_file);
    let client = Client::connect().unwrap();
    let mut content = Content::new();
    content.file(dir.join("doc.pdf")).unwrap();
    let request = client.share(&content, &ShareOptions { app_name: Some("Tests".into()), ..Default::default() }).unwrap();

    let start = Instant::now();
    let env = loop {
        if let Ok(s) = std::fs::read_to_string(&env_file)
            && s.contains("NYX_SHARE_TOKEN")
        {
            break s;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "the sheet wasn't launched");
        std::thread::sleep(Duration::from_millis(20));
    };
    let var = |name: &str| env.lines().find_map(|l| l.strip_prefix(&format!("{name}="))).unwrap().to_string();
    assert_eq!(var("NYX_SHARE_REQUEST"), request);
    let token = var("NYX_SHARE_TOKEN");

    let sheet = client.sheet().unwrap();
    assert!(sheet.get_request(&request, "wrong").is_err());
    let info: nyx_share_client::RequestInfo = serde_json::from_str(&sheet.get_request(&request, &token).unwrap()).unwrap();
    assert_eq!(info.app_name.as_deref(), Some("Tests"));
    assert_eq!(info.items[0].name, "doc.pdf");
    assert_eq!(info.items[0].mime.as_deref(), Some("application/pdf"));
    assert!(info.destinations.iter().any(|d| d.id == "save" && d.needs_folder));
    assert!(sheet.open_item(&request, &token, 0).is_err(), "only images have previews");

    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let options = format!(r#"{{"folder":{:?}}}"#, out.display().to_string());
    let job = sheet.choose(&request, &token, "save", &options).unwrap();
    assert_eq!(client.wait(&job, |_| {}).unwrap().state, JobState::Completed);
    assert_eq!(std::fs::read(out.join("doc.pdf")).unwrap(), b"%PDF");

    sheet.dismiss(&request, &token).unwrap();
    assert!(sheet.get_request(&request, &token).is_err(), "a dismissed request is gone");
}

#[test]
#[ignore]
fn keeps_destination_preferences() {
    service();
    let client = Client::connect().unwrap();
    let update = PreferencesUpdate {
        order: Some(vec!["save".into()]),
        pinned: Some(vec!["save".into()]),
        hidden: Some(vec!["clipboard".into()]),
    };
    client.set_preferences(&update).unwrap();
    let all = client.destinations(&[], 0).unwrap();
    assert_eq!(all[0].id, "save");
    assert!(all[0].pinned);
    assert!(all.iter().find(|d| d.id == "clipboard").unwrap().hidden);
    let saved = std::fs::read_to_string(service().root.join("config/nyx-share/destinations.toml")).unwrap();
    assert!(saved.contains("clipboard"));
    client.set_preferences(&PreferencesUpdate { pinned: Some(vec![]), hidden: Some(vec![]), order: Some(vec![]) }).unwrap();
}
