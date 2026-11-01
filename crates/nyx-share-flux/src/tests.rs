//! Against a fake fluxd on a socket in a scratch folder.

use super::*;
use nyx_share_core::ItemInfo;
use nyx_share_providers::SendOptions;
use std::os::unix::net::UnixListener;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex as StdMutex};

#[derive(Default)]
struct Fake {
    online: bool,
    /// What each call asked for, in order.
    calls: Vec<(String, Value)>,
    /// Each staged file's name and bytes, read while share.files runs.
    received: Vec<(String, Vec<u8>)>,
    /// How many state calls a transfer takes to finish; None leaves it waiting.
    finish_after: Option<usize>,
    fail: bool,
    states: usize,
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nyx-share-flux-{name}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn serve(dir: &Path, fake: Arc<StdMutex<Fake>>) -> PathBuf {
    let socket = dir.join("fluxd.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { break };
            let fake = fake.clone();
            std::thread::spawn(move || {
                let mut writer = conn.try_clone().unwrap();
                for line in BufReader::new(conn).lines() {
                    let Ok(line) = line else { break };
                    let req: Value = serde_json::from_str(&line).unwrap();
                    let (id, method, params) =
                        (req["id"].clone(), req["method"].as_str().unwrap().to_string(), req["params"].clone());
                    let mut f = fake.lock().unwrap();
                    f.calls.push((method.clone(), params.clone()));
                    let result = match method.as_str() {
                        "state" => {
                            f.states += 1;
                            let state = match (f.fail, f.finish_after) {
                                (true, _) => "failed",
                                (_, Some(n)) if f.states > n => "done",
                                (_, Some(_)) => "active",
                                (_, None) => "waiting",
                            };
                            let transfers: Vec<Value> = (0..f.received.len())
                                .map(|i| json!({ "id": format!("t{i}"), "state": state, "done": if state == "done" { 5 } else { 2 }, "error": "the phone refused it" }))
                                .collect();
                            json!({ "devices": [{ "id": "phone1", "name": "Pandora", "type": "phone", "paired": true, "online": f.online, "plugins": ["share", "clipboard"] },
                                                { "id": "tv1", "name": "Living room", "type": "tv", "paired": true, "online": true, "plugins": ["clipboard"] }],
                                    "transfers": transfers, "clipboard": { "secret": "never read" } })
                        }
                        "share.files" => {
                            for p in params["paths"].as_array().unwrap() {
                                let path = Path::new(p.as_str().unwrap());
                                f.received
                                    .push((path.file_name().unwrap().to_string_lossy().into(), std::fs::read(path).unwrap()));
                            }
                            json!({ "transfers": (0..f.received.len()).map(|i| format!("t{i}")).collect::<Vec<_>>() })
                        }
                        "share.text" | "share.url" if !f.online => {
                            let reply = json!({ "id": id, "error": { "code": "offline", "message": "Pandora is offline" } });
                            writeln!(writer, "{reply}").unwrap();
                            continue;
                        }
                        _ => json!({}),
                    };
                    // An event before the reply, as fluxd may send to a subscriber.
                    writeln!(writer, "{}", json!({ "event": "state", "data": {} })).unwrap();
                    writeln!(writer, "{}", json!({ "id": id, "result": result })).unwrap();
                }
            });
        }
    });
    socket
}

fn text(kind: ContentKind, s: &str) -> Payload {
    Payload {
        info: ItemInfo { kind, name: "Shared text".into(), mime: None, size: s.len() as u64, preview: None },
        data: Data::Text(s.into()),
    }
}

fn file(dir: &Path, name: &str, bytes: &[u8]) -> Payload {
    let path = dir.join(format!("src-{}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&path, bytes).unwrap();
    Payload {
        info: ItemInfo { kind: ContentKind::File, name: name.into(), mime: None, size: bytes.len() as u64, preview: None },
        data: Data::File(std::fs::File::open(&path).unwrap()),
    }
}

/// Send to the fake phone, cancelling after `cancel_after` if given.
fn send(socket: &Path, items: &[Payload], cancel_after: Option<Duration>) -> (Result<Outcome, SendError>, u64) {
    let options = SendOptions::default();
    let cancel = AtomicBool::new(false);
    let done = std::sync::atomic::AtomicU64::new(0);
    let progress = |n: u64| {
        done.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
    };
    let stage = socket.parent().unwrap().join("stage");
    let result = std::thread::scope(|s| {
        if let Some(after) = cancel_after {
            let cancel = &cancel;
            s.spawn(move || {
                std::thread::sleep(after);
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            });
        }
        let task = Task { items, options: &options, cancel: &cancel, progress: &progress, step: &|_| {} };
        FluxProvider::with_stage(socket.to_path_buf(), stage.clone()).send("flux:phone1", &task)
    });
    (result, done.into_inner())
}

#[test]
fn lists_paired_devices_that_take_shares() {
    let dir = scratch("list");
    let socket = serve(&dir, Arc::new(StdMutex::new(Fake { online: true, ..Default::default() })));
    let dests = FluxProvider::new(socket).destinations();
    assert_eq!(dests.len(), 1, "the TV has no share plugin");
    assert_eq!(dests[0].id, "flux:phone1");
    assert_eq!(dests[0].status.as_deref(), Some("Flux"));
    assert!(FluxProvider::new(dir.join("missing.sock")).destinations().is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn sends_links_text_and_files_under_their_names() {
    let dir = scratch("send");
    let fake = Arc::new(StdMutex::new(Fake { online: true, finish_after: Some(2), ..Default::default() }));
    let socket = serve(&dir, fake.clone());
    let items = [
        text(ContentKind::Url, "https://example.com/a"),
        text(ContentKind::Text, "hello"),
        file(&dir, "report.pdf", b"12345"),
        file(&dir, "report.pdf", b"abcde"),
    ];
    let (result, progress) = send(&socket, &items, None);
    let outcome = result.unwrap();
    assert_eq!(outcome.message, "Sent 4 items to Pandora");
    let f = fake.lock().unwrap();
    let methods: Vec<&str> = f.calls.iter().map(|(m, _)| m.as_str()).collect();
    assert!(methods.starts_with(&["state", "share.url", "share.text", "share.files"]), "{methods:?}");
    assert_eq!(f.calls[1].1["url"], "https://example.com/a");
    assert_eq!(f.received, vec![("report.pdf".into(), b"12345".to_vec()), ("report (2).pdf".into(), b"abcde".to_vec())]);
    assert_eq!(progress, ("https://example.com/a".len() + 5 + 10) as u64);
    let staged = Path::new(f.calls[3].1["paths"][0].as_str().unwrap()).parent().unwrap().to_path_buf();
    assert!(!staged.exists(), "the staged files are removed");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn files_for_an_offline_phone_wait_in_flux() {
    let dir = scratch("offline");
    let fake = Arc::new(StdMutex::new(Fake { online: false, ..Default::default() }));
    let socket = serve(&dir, fake.clone());
    let (result, _) = send(&socket, &[file(&dir, "a.jpg", b"x")], None);
    assert_eq!(result.unwrap().message, "Waiting in Flux: 1 file goes when Pandora is back");
    let (result, _) = send(&socket, &[text(ContentKind::Text, "hi")], None);
    assert!(matches!(result, Err(SendError::Failed { message, .. }) if message == "Pandora is offline"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn reports_a_failed_transfer_and_cancels() {
    let dir = scratch("fail");
    let fake = Arc::new(StdMutex::new(Fake { online: true, fail: true, ..Default::default() }));
    let socket = serve(&dir, fake.clone());
    let (result, _) = send(&socket, &[file(&dir, "a.jpg", b"x")], None);
    assert!(
        matches!(result, Err(SendError::Failed { message, .. }) if message == "Flux couldn't send to Pandora: the phone refused it")
    );

    let other = scratch("cancel");
    let fake = Arc::new(StdMutex::new(Fake { online: true, finish_after: Some(usize::MAX), ..Default::default() }));
    let socket = serve(&other, fake.clone());
    let (result, _) = send(&socket, &[file(&other, "big.mkv", b"x")], Some(Duration::from_millis(600)));
    assert!(matches!(result, Err(SendError::Cancelled { .. })));
    let f = fake.lock().unwrap();
    assert!(f.calls.iter().any(|(m, p)| m == "transfer.cancel" && p["id"] == "t0"), "the transfer is cancelled in Flux too");
    std::fs::remove_dir_all(dir).unwrap();
    std::fs::remove_dir_all(other).unwrap();
}

#[test]
fn copies_when_a_hard_link_cannot_be_made() {
    // /dev/shm is its own filesystem, so linking from it into the stage fails.
    let Ok(source) = tempfile_in(Path::new("/dev/shm")) else { return };
    std::fs::write(&source, b"across filesystems").unwrap();
    let stage_dir = scratch("copy");
    let options = SendOptions::default();
    let cancel = AtomicBool::new(false);
    let task = Task { items: &[], options: &options, cancel: &cancel, progress: &|_| {}, step: &|_| {} };
    let mut stage = Stage::new(&stage_dir).unwrap();
    let placed = stage.place(&std::fs::File::open(&source).unwrap(), "notes.txt", 18, &task).unwrap();
    assert_eq!(std::fs::read(&placed).unwrap(), b"across filesystems");
    use std::os::unix::fs::MetadataExt;
    assert_ne!(std::fs::metadata(&placed).unwrap().ino(), std::fs::metadata(&source).unwrap().ino());
    drop(stage);
    std::fs::remove_file(source).unwrap();
    std::fs::remove_dir_all(stage_dir).unwrap();
}

fn tempfile_in(dir: &Path) -> std::io::Result<PathBuf> {
    let path = dir.join(format!("nyx-share-flux-{}", uuid::Uuid::new_v4().simple()));
    std::fs::File::create_new(&path)?;
    Ok(path)
}
